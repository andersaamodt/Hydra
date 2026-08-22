use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};

use fs2::FileExt as _;
use hydra_domain::{AnchorId, ContentBody, MediaManifest, ObjectHead, ObjectKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::StoreError;

const LIBRARY_SCHEMA: &str = "hydra-content-library/v1";
const INDEX_SCHEMA: &str = "hydra-content-index/v1";
const HISTORY_SCHEMA: &str = "hydra-browsing-history/v1";

/// One public post or comment snapshot retained in Hydra's unencrypted library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibrarySnapshot {
    pub anchor: String,
    pub author: String,
    pub kind: ObjectKind,
    pub title: Option<String>,
    pub body: String,
    pub communities: Vec<String>,
    pub root: Option<String>,
    pub parent: Option<String>,
    pub external_root: Option<String>,
    pub external_parent: Option<String>,
    pub external_source: Option<String>,
    pub protocol: String,
    pub protocol_identifier: Option<String>,
    pub source_url: Option<String>,
    pub created_at: u64,
    pub edited_at: u64,
}

impl LibrarySnapshot {
    #[must_use]
    pub fn from_head(
        head: &ObjectHead,
        protocol: impl Into<String>,
        protocol_identifier: Option<String>,
    ) -> Self {
        let external = head.external_source.as_ref();
        Self {
            anchor: head.anchor.as_str().to_owned(),
            author: head.author.as_str().to_owned(),
            kind: head.kind,
            title: head.title.clone(),
            body: head.body.as_str().to_owned(),
            communities: head
                .communities
                .iter()
                .map(|community| community.as_str().to_owned())
                .collect(),
            root: head.root.as_ref().map(|anchor| anchor.as_str().to_owned()),
            parent: head
                .parent
                .as_ref()
                .map(|anchor| anchor.as_str().to_owned()),
            external_root: head.external_root.as_ref().map(external_id),
            external_parent: head.external_parent.as_ref().map(external_id),
            external_source: external.map(external_id),
            protocol: protocol.into(),
            protocol_identifier,
            source_url: external
                .map(|identifier| identifier.canonical.as_str())
                .filter(|value| value.starts_with("https://") || value.starts_with("http://"))
                .map(str::to_owned),
            created_at: head.edited_at,
            edited_at: head.edited_at,
        }
    }

    /// Reconstructs a Hydra object from the canonical text file.
    ///
    /// # Errors
    ///
    /// Returns an error when the human-edited fields no longer satisfy Hydra's
    /// public content contracts.
    pub fn to_head(&self) -> Result<ObjectHead, StoreError> {
        let body = match self.kind {
            ObjectKind::Post => ContentBody::parse_post(self.body.clone())?,
            ObjectKind::Comment | ObjectKind::Norm => ContentBody::parse(self.body.clone())?,
        };
        let head = ObjectHead {
            anchor: AnchorId::parse(self.anchor.clone())?,
            author: hydra_domain::NostrPublicKey::parse(self.author.clone())?,
            kind: self.kind,
            title: self.title.clone(),
            body,
            communities: self
                .communities
                .iter()
                .cloned()
                .map(hydra_domain::CommunityKey::parse)
                .collect::<Result<Vec<_>, _>>()?,
            root: self.root.as_deref().map(AnchorId::parse).transpose()?,
            parent: self.parent.as_deref().map(AnchorId::parse).transpose()?,
            external_root: parse_external(self.external_root.as_deref())?,
            external_parent: parse_external(self.external_parent.as_deref())?,
            external_source: parse_external(self.external_source.as_deref())?,
            edited_at: self.edited_at,
        };
        head.validate()?;
        Ok(head)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryInteraction {
    pub action: String,
    pub actor: String,
    pub occurred_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryTombstone {
    pub requested_at: u64,
    pub actor: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LibraryFrontmatter {
    schema: String,
    kind: ObjectKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    anchor: String,
    author: String,
    protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    protocol_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
    #[serde(default)]
    communities: Vec<String>,
    created_at: u64,
    edited_at: u64,
    recorded_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    external_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    external_parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    external_source: Option<String>,
    content_sha256: String,
    #[serde(default)]
    interactions: Vec<LibraryInteraction>,
    #[serde(default)]
    tombstones: Vec<LibraryTombstone>,
}

#[derive(Debug, Clone)]
struct LibraryDocument {
    frontmatter: LibraryFrontmatter,
    body: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LibraryIndex {
    schema: String,
    #[serde(default)]
    items: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct HistoryFile {
    schema: String,
    date: String,
    #[serde(default)]
    entries: Vec<HistoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HistoryEntry {
    anchor: String,
    action: String,
    occurred_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

/// Hydra's searchable, unencrypted, atomic YAML+Markdown content store.
#[derive(Debug, Clone)]
pub struct ContentLibrary {
    root: PathBuf,
}

impl ContentLibrary {
    #[must_use]
    pub fn new(durable_root: impl AsRef<Path>) -> Self {
        Self {
            root: durable_root.as_ref().join("library"),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Atomically stores a canonical snapshot. A changed prior body is copied
    /// into `revisions/` and the overwritten file is included in the day's
    /// automatic text backup before the new canonical file is installed.
    ///
    /// # Errors
    ///
    /// Returns an error when the library is unsafe, invalid, or cannot be synced.
    pub fn record_snapshot(
        &self,
        snapshot: &LibrarySnapshot,
        recorded_at: u64,
        history_action: Option<&str>,
    ) -> Result<PathBuf, StoreError> {
        validate_snapshot(snapshot)?;
        ensure_directory(&self.root)?;
        let _lock = self.exclusive_lock()?;
        let relative = content_relative_path(snapshot);
        let path = self.root.join(&relative);
        let previous = read_document_if_exists(&path)?;
        let interactions = previous.as_ref().map_or_else(Vec::new, |document| {
            document.frontmatter.interactions.clone()
        });
        let mut tombstones = previous
            .as_ref()
            .map_or_else(Vec::new, |document| document.frontmatter.tombstones.clone());
        for tombstone in self.orphan_tombstones(&snapshot.anchor)? {
            if !tombstones.contains(&tombstone) {
                tombstones.push(tombstone);
            }
        }
        let created_at = previous.as_ref().map_or(snapshot.created_at, |document| {
            document.frontmatter.created_at.min(snapshot.created_at)
        });
        let document = LibraryDocument {
            frontmatter: LibraryFrontmatter {
                schema: LIBRARY_SCHEMA.to_owned(),
                kind: snapshot.kind,
                title: snapshot.title.clone(),
                anchor: snapshot.anchor.clone(),
                author: snapshot.author.clone(),
                protocol: snapshot.protocol.clone(),
                protocol_identifier: snapshot.protocol_identifier.clone().or_else(|| {
                    previous
                        .as_ref()
                        .and_then(|document| document.frontmatter.protocol_identifier.clone())
                }),
                source_url: snapshot.source_url.clone().or_else(|| {
                    previous
                        .as_ref()
                        .and_then(|document| document.frontmatter.source_url.clone())
                }),
                communities: snapshot.communities.clone(),
                created_at,
                edited_at: snapshot.edited_at,
                recorded_at: previous
                    .as_ref()
                    .map_or(recorded_at, |document| document.frontmatter.recorded_at),
                root: snapshot.root.clone(),
                parent: snapshot.parent.clone(),
                external_root: snapshot.external_root.clone(),
                external_parent: snapshot.external_parent.clone(),
                external_source: snapshot.external_source.clone(),
                content_sha256: content_hash(snapshot.title.as_deref(), &snapshot.body),
                interactions,
                tombstones,
            },
            body: render_markdown(snapshot.title.as_deref(), &snapshot.body),
        };
        if let Some(previous) = previous
            && previous.frontmatter.content_sha256 != document.frontmatter.content_sha256
        {
            let revision = path
                .parent()
                .ok_or_else(|| {
                    StoreError::InvalidSettings("library path has no parent".to_owned())
                })?
                .join("revisions")
                .join(format!(
                    "{}-{}.md",
                    previous.frontmatter.edited_at,
                    &previous.frontmatter.content_sha256[..12]
                ));
            if !revision.exists() {
                write_atomic(&revision, &serialize_document(&previous)?)?;
            }
        }
        self.write_canonical(&path, &serialize_document(&document)?, recorded_at)?;
        self.update_index(&snapshot.anchor, &relative, recorded_at)?;
        if let Some(action) = history_action {
            self.record_history(
                HistoryEntry {
                    anchor: snapshot.anchor.clone(),
                    action: action.to_owned(),
                    occurred_at: recorded_at,
                    actor: None,
                    detail: None,
                },
                action == "viewed",
            )?;
        }
        Ok(path)
    }

    /// Records a local action in both the canonical frontmatter and daily YAML history.
    ///
    /// # Errors
    ///
    /// Deletions observed before their content are kept in an orphan tombstone
    /// directory and folded into the canonical document if it arrives later.
    pub fn record_interaction(
        &self,
        anchor: &str,
        actor: &str,
        action: &str,
        detail: Option<String>,
        occurred_at: u64,
    ) -> Result<(), StoreError> {
        let _lock = self.exclusive_lock()?;
        let path = self.path_for_anchor(anchor)?.ok_or(StoreError::NotFound)?;
        let mut document = read_document(&path)?;
        let interaction = LibraryInteraction {
            action: action.to_owned(),
            actor: actor.to_owned(),
            occurred_at,
            detail: detail.clone(),
        };
        if !document.frontmatter.interactions.contains(&interaction) {
            document.frontmatter.interactions.push(interaction);
        }
        self.write_canonical(&path, &serialize_document(&document)?, occurred_at)?;
        self.record_history(
            HistoryEntry {
                anchor: anchor.to_owned(),
                action: action.to_owned(),
                occurred_at,
                actor: Some(actor.to_owned()),
                detail,
            },
            false,
        )
    }

    /// Adds a non-destructive local or remote deletion record.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotFound`] when the target was not retained first.
    pub fn record_tombstone(
        &self,
        anchor: &str,
        actor: &str,
        reason: &str,
        requested_at: u64,
    ) -> Result<(), StoreError> {
        ensure_directory(&self.root)?;
        let _lock = self.exclusive_lock()?;
        let tombstone = LibraryTombstone {
            requested_at,
            actor: actor.to_owned(),
            reason: reason.to_owned(),
            status: "requested; retained locally".to_owned(),
        };
        let Some(path) = self.path_for_anchor(anchor)? else {
            let orphan = self
                .root
                .join("tombstones")
                .join(stable_digest(anchor))
                .join(format!("{requested_at}.yaml"));
            write_yaml_atomic(&orphan, &tombstone)?;
            return self.record_history(
                HistoryEntry {
                    anchor: anchor.to_owned(),
                    action: "tombstone".to_owned(),
                    occurred_at: requested_at,
                    actor: Some(actor.to_owned()),
                    detail: (!reason.is_empty()).then(|| reason.to_owned()),
                },
                false,
            );
        };
        let mut document = read_document(&path)?;
        if !document.frontmatter.tombstones.contains(&tombstone) {
            document.frontmatter.tombstones.push(tombstone.clone());
        }
        let tombstone_path = path
            .parent()
            .ok_or_else(|| StoreError::InvalidSettings("library path has no parent".to_owned()))?
            .join("tombstones")
            .join(format!("{requested_at}.yaml"));
        write_yaml_atomic(&tombstone_path, &tombstone)?;
        self.write_canonical(&path, &serialize_document(&document)?, requested_at)?;
        self.record_history(
            HistoryEntry {
                anchor: anchor.to_owned(),
                action: "tombstone".to_owned(),
                occurred_at: requested_at,
                actor: Some(actor.to_owned()),
                detail: (!reason.is_empty()).then(|| reason.to_owned()),
            },
            false,
        )
    }

    /// Writes a searchable YAML sidecar for preserved content-addressed media.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest is invalid or cannot be stored.
    pub fn record_media(
        &self,
        manifest: &MediaManifest,
        recorded_at: u64,
    ) -> Result<(), StoreError> {
        ensure_directory(&self.root)?;
        let _lock = self.exclusive_lock()?;
        manifest.validate()?;
        let path = self
            .root
            .join("media")
            .join(format!("{}.yaml", manifest.sha256));
        self.write_canonical(
            &path,
            &serde_yaml::to_string(manifest)?.into_bytes(),
            recorded_at,
        )
    }

    /// Returns every valid canonical Hydra object in the library. Revision,
    /// tombstone, history, backup, and generic Nostr files are not replayed.
    ///
    /// # Errors
    ///
    /// Returns an error for an unreadable index or malformed canonical file.
    pub fn load_hydra_heads(&self) -> Result<Vec<(ObjectHead, u64)>, StoreError> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let _lock = self.exclusive_lock()?;
        let index = self.load_index()?;
        let mut heads = Vec::new();
        for relative in index.items.values() {
            let document = read_document(&self.root.join(relative))?;
            if document.frontmatter.protocol != "hydra" {
                continue;
            }
            let snapshot = snapshot_from_document(&document);
            heads.push((snapshot.to_head()?, document.frontmatter.recorded_at));
        }
        heads.sort_by_key(|(head, _)| head.edited_at);
        Ok(heads)
    }

    fn update_index(
        &self,
        anchor: &str,
        relative: &Path,
        recorded_at: u64,
    ) -> Result<(), StoreError> {
        let mut index = self.load_index()?;
        INDEX_SCHEMA.clone_into(&mut index.schema);
        index
            .items
            .insert(anchor.to_owned(), relative.to_string_lossy().into_owned());
        self.write_canonical(
            &self.root.join("index.yaml"),
            &serde_yaml::to_string(&index)?.into_bytes(),
            recorded_at,
        )
    }

    fn exclusive_lock(&self) -> Result<File, StoreError> {
        ensure_directory(&self.root)?;
        let path = self.root.join("library.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        lock.lock_exclusive()?;
        Ok(lock)
    }

    fn load_index(&self) -> Result<LibraryIndex, StoreError> {
        let path = self.root.join("index.yaml");
        if !path.exists() {
            return Ok(LibraryIndex {
                schema: INDEX_SCHEMA.to_owned(),
                items: BTreeMap::new(),
            });
        }
        let index: LibraryIndex = serde_yaml::from_slice(&fs::read(path)?)?;
        if index.schema != INDEX_SCHEMA {
            return Err(StoreError::InvalidSettings(
                "unsupported content-library index schema".to_owned(),
            ));
        }
        Ok(index)
    }

    fn path_for_anchor(&self, anchor: &str) -> Result<Option<PathBuf>, StoreError> {
        Ok(self
            .load_index()?
            .items
            .get(anchor)
            .map(|relative| self.root.join(relative)))
    }

    fn orphan_tombstones(&self, anchor: &str) -> Result<Vec<LibraryTombstone>, StoreError> {
        let directory = self.root.join("tombstones").join(stable_digest(anchor));
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut tombstones = Vec::new();
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.path().extension().and_then(|value| value.to_str()) == Some("yaml")
            {
                tombstones.push(serde_yaml::from_slice(&fs::read(entry.path())?)?);
            }
        }
        tombstones.sort_by_key(|item: &LibraryTombstone| item.requested_at);
        Ok(tombstones)
    }

    fn record_history(
        &self,
        entry: HistoryEntry,
        deduplicate_daily: bool,
    ) -> Result<(), StoreError> {
        let date = utc_date(entry.occurred_at);
        let path = self.root.join("history").join(format!("{date}.yaml"));
        let mut history = if path.exists() {
            serde_yaml::from_slice::<HistoryFile>(&fs::read(&path)?)?
        } else {
            HistoryFile {
                schema: HISTORY_SCHEMA.to_owned(),
                date: date.clone(),
                entries: Vec::new(),
            }
        };
        if history.schema != HISTORY_SCHEMA || history.date != date {
            return Err(StoreError::InvalidSettings(
                "browsing-history file has an unsupported schema or date".to_owned(),
            ));
        }
        let duplicate = deduplicate_daily
            && history.entries.iter().any(|existing| {
                existing.anchor == entry.anchor
                    && existing.action == entry.action
                    && existing.actor == entry.actor
            });
        if !duplicate {
            history.entries.push(entry);
            self.write_canonical(
                &path,
                &serde_yaml::to_string(&history)?.into_bytes(),
                history.entries.last().map_or(0, |entry| entry.occurred_at),
            )?;
        }
        Ok(())
    }

    fn write_canonical(&self, path: &Path, bytes: &[u8], timestamp: u64) -> Result<(), StoreError> {
        if path.exists() && fs::read(path)? == bytes {
            return Ok(());
        }
        let existed = path.exists();
        let relative = path.strip_prefix(&self.root).map_err(|_| {
            StoreError::InvalidSettings("library write escaped its root".to_owned())
        })?;
        let backup = self
            .root
            .join("backups")
            .join(utc_date(timestamp))
            .join(relative);
        if existed && !backup.exists() {
            write_atomic(&backup, &fs::read(path)?)?;
        }
        write_atomic(path, bytes)?;
        if !existed && !backup.exists() {
            write_atomic(&backup, bytes)?;
        }
        Ok(())
    }
}

fn validate_snapshot(snapshot: &LibrarySnapshot) -> Result<(), StoreError> {
    if snapshot.anchor.trim().is_empty()
        || snapshot.author.trim().is_empty()
        || snapshot.protocol.trim().is_empty()
        || snapshot.edited_at < snapshot.created_at
    {
        return Err(StoreError::InvalidSettings(
            "content-library snapshot is incomplete".to_owned(),
        ));
    }
    Ok(())
}

fn content_relative_path(snapshot: &LibrarySnapshot) -> PathBuf {
    let digest = stable_digest(&snapshot.anchor);
    match snapshot.kind {
        ObjectKind::Post => PathBuf::from("posts").join(digest).join("post.md"),
        ObjectKind::Comment => {
            if let Some(root) = &snapshot.root {
                PathBuf::from("posts")
                    .join(stable_digest(root))
                    .join("comments")
                    .join(digest)
                    .join("comment.md")
            } else {
                PathBuf::from("comments").join(digest).join("comment.md")
            }
        }
        ObjectKind::Norm => PathBuf::from("norms").join(digest).join("norm.md"),
    }
}

fn stable_digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn content_hash(title: Option<&str>, body: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(title.unwrap_or_default().as_bytes());
    digest.update([0]);
    digest.update(body.as_bytes());
    format!("{:x}", digest.finalize())
}

fn render_markdown(title: Option<&str>, body: &str) -> String {
    title.map_or_else(|| body.to_owned(), |title| format!("# {title}\n\n{body}"))
}

fn serialize_document(document: &LibraryDocument) -> Result<Vec<u8>, StoreError> {
    let yaml = serde_yaml::to_string(&document.frontmatter)?;
    Ok(format!(
        "---\n{}---\n\n{}\n",
        yaml.trim_start_matches("---\n"),
        document.body
    )
    .into_bytes())
}

fn read_document_if_exists(path: &Path) -> Result<Option<LibraryDocument>, StoreError> {
    if path.exists() {
        read_document(path).map(Some)
    } else {
        Ok(None)
    }
}

fn read_document(path: &Path) -> Result<LibraryDocument, StoreError> {
    let value = fs::read_to_string(path)?;
    let rest = value.strip_prefix("---\n").ok_or_else(|| {
        StoreError::InvalidSettings("library Markdown is missing YAML frontmatter".to_owned())
    })?;
    let (yaml, markdown) = rest.split_once("\n---\n").ok_or_else(|| {
        StoreError::InvalidSettings("library Markdown has unterminated YAML frontmatter".to_owned())
    })?;
    let frontmatter: LibraryFrontmatter = serde_yaml::from_str(yaml)?;
    if frontmatter.schema != LIBRARY_SCHEMA {
        return Err(StoreError::InvalidSettings(
            "unsupported content-library document schema".to_owned(),
        ));
    }
    let markdown = markdown.trim_start_matches('\n').trim_end_matches('\n');
    let body = body_from_markdown(frontmatter.title.as_deref(), markdown);
    if content_hash(frontmatter.title.as_deref(), &body) != frontmatter.content_sha256 {
        return Err(StoreError::InvalidSettings(format!(
            "content-library hash mismatch for {}",
            frontmatter.anchor
        )));
    }
    Ok(LibraryDocument {
        frontmatter,
        body: markdown.to_owned(),
    })
}

fn snapshot_from_document(document: &LibraryDocument) -> LibrarySnapshot {
    let title = document.frontmatter.title.clone();
    let body = body_from_markdown(title.as_deref(), &document.body);
    LibrarySnapshot {
        anchor: document.frontmatter.anchor.clone(),
        author: document.frontmatter.author.clone(),
        kind: document.frontmatter.kind,
        title,
        body,
        communities: document.frontmatter.communities.clone(),
        root: document.frontmatter.root.clone(),
        parent: document.frontmatter.parent.clone(),
        external_root: document.frontmatter.external_root.clone(),
        external_parent: document.frontmatter.external_parent.clone(),
        external_source: document.frontmatter.external_source.clone(),
        protocol: document.frontmatter.protocol.clone(),
        protocol_identifier: document.frontmatter.protocol_identifier.clone(),
        source_url: document.frontmatter.source_url.clone(),
        created_at: document.frontmatter.created_at,
        edited_at: document.frontmatter.edited_at,
    }
}

fn body_from_markdown(title: Option<&str>, markdown: &str) -> String {
    title
        .and_then(|title| markdown.strip_prefix(&format!("# {title}\n\n")))
        .unwrap_or(markdown)
        .to_owned()
}

fn write_yaml_atomic(path: &Path, value: &impl Serialize) -> Result<(), StoreError> {
    write_atomic(path, &serde_yaml::to_string(value)?.into_bytes())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let parent = path.parent().ok_or_else(|| {
        StoreError::InvalidSettings("library destination has no parent".to_owned())
    })?;
    ensure_directory(parent)?;
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StoreError::InvalidSettings(
                "library destination must be a regular file".to_owned(),
            ));
        }
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| StoreError::Io(error.error))?;
    sync_directory(parent)?;
    Ok(())
}

fn ensure_directory(path: &Path) -> Result<(), StoreError> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(StoreError::InvalidSettings(
                "content library requires real directories".to_owned(),
            ));
        }
        return Ok(());
    }
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StoreError::InvalidSettings(
            "content library requires real directories".to_owned(),
        ));
    }
    sync_directory(path.parent().unwrap_or(path))
}

fn sync_directory(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

fn external_id(identifier: &hydra_domain::ExternalId) -> String {
    format!("{}:{}", identifier.system, identifier.canonical)
}

fn parse_external(value: Option<&str>) -> Result<Option<hydra_domain::ExternalId>, StoreError> {
    value
        .map(|value| {
            value
                .split_once(':')
                .ok_or_else(|| {
                    StoreError::InvalidSettings("external identifier is malformed".to_owned())
                })
                .and_then(|(system, canonical)| {
                    hydra_domain::ExternalId::new(system, canonical).map_err(StoreError::from)
                })
        })
        .transpose()
}

fn utc_date(timestamp: u64) -> String {
    let days = i64::try_from(timestamp / 86_400).unwrap_or(i64::MAX);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use hydra_domain::{CommunityKey, NostrPublicKey};

    use super::*;

    fn snapshot(body: &str, edited_at: u64) -> LibrarySnapshot {
        LibrarySnapshot {
            anchor: "anchor-1".to_owned(),
            author: "a".repeat(64),
            kind: ObjectKind::Post,
            title: Some("A durable post".to_owned()),
            body: body.to_owned(),
            communities: vec!["science".to_owned()],
            root: None,
            parent: None,
            external_root: None,
            external_parent: None,
            external_source: None,
            protocol: "hydra".to_owned(),
            protocol_identifier: Some(format!("event-{edited_at}")),
            source_url: None,
            created_at: 10,
            edited_at,
        }
    }

    #[test]
    fn canonical_markdown_keeps_revisions_interactions_history_and_backups() {
        let root = tempfile::tempdir().unwrap();
        let library = ContentLibrary::new(root.path());
        let first = snapshot("First body", 10);
        let path = library
            .record_snapshot(&first, 10, Some("authored"))
            .unwrap();
        library
            .record_interaction("anchor-1", "persona-1", "upvote", Some("+".to_owned()), 20)
            .unwrap();
        library
            .record_snapshot(&snapshot("Second body", 30), 30, None)
            .unwrap();

        let markdown = fs::read_to_string(&path).unwrap();
        assert!(markdown.starts_with("---\n"));
        assert!(markdown.contains("# A durable post\n\nSecond body"));
        assert!(markdown.contains("action: upvote"));
        let directory = path.parent().unwrap();
        assert_eq!(
            fs::read_dir(directory.join("revisions")).unwrap().count(),
            1
        );
        assert!(library.root().join("history/1970-01-01.yaml").is_file());
        let relative = path.strip_prefix(library.root()).unwrap();
        let backup = library.root().join("backups/1970-01-01").join(relative);
        assert!(backup.is_file());
        assert!(fs::read_to_string(backup).unwrap().contains("First body"));
    }

    #[test]
    fn canonical_hydra_markdown_can_recover_a_missing_operational_index() {
        let root = tempfile::tempdir().unwrap();
        let library = ContentLibrary::new(root.path());
        let author = NostrPublicKey::parse("b".repeat(64)).unwrap();
        let head = ObjectHead {
            anchor: AnchorId::parse("anchor-2").unwrap(),
            author,
            kind: ObjectKind::Post,
            title: Some("Recovered".to_owned()),
            body: ContentBody::parse_post("Readable text").unwrap(),
            communities: vec![CommunityKey::parse("science").unwrap()],
            root: None,
            parent: None,
            external_root: None,
            external_parent: None,
            external_source: None,
            edited_at: 42,
        };
        library
            .record_snapshot(&LibrarySnapshot::from_head(&head, "hydra", None), 42, None)
            .unwrap();
        let recovered = library.load_hydra_heads().unwrap();
        assert_eq!(recovered, vec![(head, 42)]);
    }

    #[test]
    fn deletion_observed_before_content_becomes_a_non_destructive_tombstone() {
        let root = tempfile::tempdir().unwrap();
        let library = ContentLibrary::new(root.path());
        library
            .record_tombstone("anchor-1", "remote-author", "withdrawn", 20)
            .unwrap();
        let path = library
            .record_snapshot(&snapshot("Still retained", 10), 30, None)
            .unwrap();
        let markdown = fs::read_to_string(path).unwrap();

        assert!(markdown.contains("status: requested; retained locally"));
        assert!(markdown.contains("reason: withdrawn"));
        assert!(markdown.contains("Still retained"));
    }

    #[test]
    fn utc_history_names_are_stable_without_locale_state() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(1_779_494_400), "2026-05-23");
    }
}
