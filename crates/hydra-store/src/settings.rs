use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{StoreError, write_bytes_atomic};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessProbe {
    pub last_tested_at: Option<u64>,
    pub last_success_at: Option<u64>,
    pub ready: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuitySettings {
    #[serde(default = "default_enabled")]
    pub big_stick_enabled: bool,
    #[serde(default = "default_enabled")]
    pub reddacted_enabled: bool,
    #[serde(default = "default_archive_level")]
    pub big_stick_archive_level: String,
    #[serde(default = "default_archive_level")]
    pub reddacted_archive_level: String,
    #[serde(default)]
    pub replication_threshold: Option<usize>,
    #[serde(default)]
    pub preferred_gateway_template: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaRelaySettings {
    pub read: Vec<String>,
    pub write: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossLinkSettings {
    #[serde(default = "default_enabled")]
    pub book_club_enabled: bool,
}

impl Default for CrossLinkSettings {
    fn default() -> Self {
        Self {
            book_club_enabled: true,
        }
    }
}

impl Default for ContinuitySettings {
    fn default() -> Self {
        Self {
            big_stick_enabled: true,
            reddacted_enabled: true,
            big_stick_archive_level: default_archive_level(),
            reddacted_archive_level: default_archive_level(),
            replication_threshold: None,
            preferred_gateway_template: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommunityAppearanceSetting {
    pub url: String,
    pub sha256: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub alt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub relays: Vec<String>,
    #[serde(default)]
    pub persona_relays: BTreeMap<String, PersonaRelaySettings>,
    #[serde(default = "default_inbox_relays")]
    pub inbox_relays: Vec<String>,
    pub replication_threshold: usize,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_accent")]
    pub accent: String,
    #[serde(default)]
    pub onboarding_complete: bool,
    #[serde(default)]
    pub active_persona_id: Option<String>,
    #[serde(default)]
    pub last_backup_at: Option<u64>,
    #[serde(default)]
    pub relay_probe: ReadinessProbe,
    #[serde(default)]
    pub reddit_probe: ReadinessProbe,
    #[serde(default)]
    pub crosspost_default: bool,
    #[serde(default)]
    pub cross_links: CrossLinkSettings,
    #[serde(default)]
    pub persona_crosspost_defaults: BTreeMap<String, bool>,
    #[serde(default)]
    pub community_crosspost_defaults: BTreeMap<String, bool>,
    #[serde(default)]
    pub content_crosspost_defaults: BTreeMap<String, bool>,
    #[serde(default)]
    pub reddit_export_imports: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub local_topic_assignments: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    #[serde(default)]
    pub community_appearances: BTreeMap<String, CommunityAppearanceSetting>,
    #[serde(default = "default_media_copy_enabled")]
    pub media_copy_enabled: bool,
    #[serde(default = "default_max_media_bytes")]
    pub max_media_bytes: u64,
    #[serde(default)]
    pub persona_blob_servers: BTreeMap<String, Vec<String>>,
    #[serde(default = "default_feed_source_weights")]
    pub feed_source_weights: BTreeMap<String, u8>,
    #[serde(default = "default_spam_threshold")]
    pub spam_filter_threshold: u8,
    #[serde(default = "default_remote_media_policy")]
    pub remote_media_policy: String,
    #[serde(default)]
    pub continuity: ContinuitySettings,
}

const fn default_enabled() -> bool {
    true
}

fn default_archive_level() -> String {
    "ancestors".to_owned()
}

const fn default_media_copy_enabled() -> bool {
    true
}

const fn default_max_media_bytes() -> u64 {
    25 * 1024 * 1024
}

fn default_theme() -> String {
    "light".to_owned()
}

fn default_accent() -> String {
    "stone-blue".to_owned()
}

fn default_inbox_relays() -> Vec<String> {
    vec!["wss://auth.nostr1.com".to_owned()]
}

fn default_feed_source_weights() -> BTreeMap<String, u8> {
    ["followed", "communities", "replies", "revisit"]
        .into_iter()
        .map(|source| (source.to_owned(), 100))
        .collect()
}

const fn default_spam_threshold() -> u8 {
    100
}

fn default_remote_media_policy() -> String {
    "on_demand".to_owned()
}

fn valid_persona_relays(value: &PersonaRelaySettings) -> bool {
    !value.read.is_empty()
        && !value.write.is_empty()
        && value.read.len() <= 32
        && value.write.len() <= 32
        && value.read.iter().chain(&value.write).all(|relay| {
            (relay.starts_with("wss://") || relay.starts_with("ws://"))
                && relay.len() <= 2_048
                && !relay.chars().any(char::is_whitespace)
        })
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            relays: vec![
                "wss://relay.damus.io".to_owned(),
                "wss://nos.lol".to_owned(),
                "wss://relay.primal.net".to_owned(),
            ],
            persona_relays: BTreeMap::new(),
            inbox_relays: default_inbox_relays(),
            replication_threshold: 2,
            theme: default_theme(),
            accent: default_accent(),
            onboarding_complete: false,
            active_persona_id: None,
            last_backup_at: None,
            relay_probe: ReadinessProbe::default(),
            reddit_probe: ReadinessProbe::default(),
            crosspost_default: false,
            cross_links: CrossLinkSettings::default(),
            persona_crosspost_defaults: BTreeMap::new(),
            community_crosspost_defaults: BTreeMap::new(),
            content_crosspost_defaults: BTreeMap::new(),
            reddit_export_imports: BTreeMap::new(),
            local_topic_assignments: BTreeMap::new(),
            community_appearances: BTreeMap::new(),
            media_copy_enabled: default_media_copy_enabled(),
            max_media_bytes: default_max_media_bytes(),
            persona_blob_servers: BTreeMap::new(),
            feed_source_weights: default_feed_source_weights(),
            spam_filter_threshold: default_spam_threshold(),
            remote_media_policy: default_remote_media_policy(),
            continuity: ContinuitySettings::default(),
        }
    }
}

impl Settings {
    #[must_use]
    pub fn read_relays_for(&self, persona: hydra_domain::PersonaId) -> &[String] {
        self.persona_relays
            .get(&persona.to_string())
            .map_or(&self.relays, |value| &value.read)
    }

    #[must_use]
    pub fn write_relays_for(&self, persona: hydra_domain::PersonaId) -> &[String] {
        self.persona_relays
            .get(&persona.to_string())
            .map_or(&self.relays, |value| &value.write)
    }

    fn validate(&self) -> Result<(), StoreError> {
        self.validate_relays()?;
        self.validate_preferences()?;
        self.validate_local_records()?;
        self.validate_continuity()?;
        validate_lens_settings(self)
    }

    fn validate_relays(&self) -> Result<(), StoreError> {
        if self.relays.is_empty() {
            return Err(StoreError::InvalidSettings(
                "at least one relay is required".to_owned(),
            ));
        }
        if self.inbox_relays.is_empty() || self.inbox_relays.len() > 3 {
            return Err(StoreError::InvalidSettings(
                "NIP-17 inbox relay count must be between one and three".to_owned(),
            ));
        }
        if !self.persona_relays.values().all(valid_persona_relays) {
            return Err(StoreError::InvalidSettings(
                "persona relay lists require 1-32 valid WebSocket URLs per direction".to_owned(),
            ));
        }
        if self.replication_threshold == 0 || self.replication_threshold > self.relays.len() {
            return Err(StoreError::InvalidSettings(
                "replication threshold must be between one and the relay count".to_owned(),
            ));
        }
        if self
            .persona_relays
            .values()
            .any(|value| self.replication_threshold > value.write.len())
        {
            return Err(StoreError::InvalidSettings(
                "replication threshold must fit every persona write-relay list".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_preferences(&self) -> Result<(), StoreError> {
        if !matches!(self.theme.as_str(), "system" | "light" | "dark") {
            return Err(StoreError::InvalidSettings(
                "theme must be system, light, or dark".to_owned(),
            ));
        }
        if !matches!(
            self.accent.as_str(),
            "stone-blue" | "indigo" | "violet" | "terracotta" | "moss"
        ) {
            return Err(StoreError::InvalidSettings(
                "accent must be stone-blue, indigo, violet, terracotta, or moss".to_owned(),
            ));
        }
        if self
            .content_crosspost_defaults
            .keys()
            .any(|key| !matches!(key.as_str(), "post" | "comment"))
        {
            return Err(StoreError::InvalidSettings(
                "crosspost content defaults support only post or comment".to_owned(),
            ));
        }
        if self.max_media_bytes == 0
            || self.max_media_bytes > hydra_domain::MediaManifest::MAX_BYTES
        {
            return Err(StoreError::InvalidSettings(format!(
                "media limit must be between 1 and {} bytes",
                hydra_domain::MediaManifest::MAX_BYTES
            )));
        }
        Ok(())
    }

    fn validate_local_records(&self) -> Result<(), StoreError> {
        if self.community_appearances.len() > 10_000
            || self.community_appearances.iter().any(|(topic, image)| {
                hydra_domain::CommunityKey::parse(topic).is_err()
                    || !image.url.starts_with("https://")
                    || image.url.len() > 2_048
                    || image.sha256.len() != 64
                    || !image.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
                    || !matches!(
                        image.mime_type.as_str(),
                        "image/png" | "image/jpeg" | "image/webp"
                    )
                    || image.width == 0
                    || image.height == 0
                    || image.width > 4_096
                    || image.height > 4_096
                    || image.alt.trim().is_empty()
                    || image.alt.len() > 280
            })
        {
            return Err(StoreError::InvalidSettings(
                "community appearance metadata is invalid".to_owned(),
            ));
        }
        if self.reddit_export_imports.values().any(|items| {
            items.len() > 500_000
                || items.iter().any(|item| {
                    item.len() < 4
                        || item.len() > 35
                        || !(item.starts_with("t1_") || item.starts_with("t3_"))
                        || !item[3..].bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
        }) {
            return Err(StoreError::InvalidSettings(
                "Reddit export import history is invalid".to_owned(),
            ));
        }
        if self.local_topic_assignments.values().any(|assignments| {
            assignments.len() > 100_000
                || assignments.iter().any(|(event_id, topics)| {
                    event_id.len() != 64
                        || !event_id.bytes().all(|byte| byte.is_ascii_hexdigit())
                        || topics.is_empty()
                        || topics.len() > hydra_domain::ObjectHead::MAX_COMMUNITIES
                        || topics
                            .iter()
                            .any(|topic| hydra_domain::CommunityKey::parse(topic).is_err())
                })
        }) {
            return Err(StoreError::InvalidSettings(
                "local topic assignments are invalid".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_continuity(&self) -> Result<(), StoreError> {
        if self
            .continuity
            .preferred_gateway_template
            .as_deref()
            .is_some_and(|value| {
                !value.starts_with("https://")
                    || !value.contains("{identifier}")
                    || value.len() > 2_048
                    || value.chars().any(char::is_whitespace)
            })
        {
            return Err(StoreError::InvalidSettings(
                "preferred gateway must be an HTTPS template containing {identifier}".to_owned(),
            ));
        }
        if !matches!(
            self.continuity.big_stick_archive_level.as_str(),
            "item" | "ancestors" | "visible_siblings" | "loaded_thread"
        ) || !matches!(
            self.continuity.reddacted_archive_level.as_str(),
            "item" | "ancestors" | "visible_siblings" | "loaded_thread"
        ) {
            return Err(StoreError::InvalidSettings(
                "continuity archive level must be item, ancestors, visible_siblings, or loaded_thread"
                    .to_owned(),
            ));
        }
        if self.continuity.replication_threshold.is_some_and(|value| {
            value == 0
                || value > self.relays.len()
                || self
                    .persona_relays
                    .values()
                    .any(|relays| value > relays.write.len())
        }) {
            return Err(StoreError::InvalidSettings(
                "continuity replication threshold must fit the configured relay set".to_owned(),
            ));
        }
        Ok(())
    }
}

fn validate_lens_settings(settings: &Settings) -> Result<(), StoreError> {
    if settings.feed_source_weights.len() != 4
        || settings.feed_source_weights.iter().any(|(source, weight)| {
            !matches!(
                source.as_str(),
                "followed" | "communities" | "replies" | "revisit"
            ) || *weight > 200
        })
    {
        return Err(StoreError::InvalidSettings(
            "feed source weights must define followed, communities, replies, and revisit from 0-200"
                .to_owned(),
        ));
    }
    if !matches!(settings.remote_media_policy.as_str(), "never" | "on_demand") {
        return Err(StoreError::InvalidSettings(
            "remote media policy must be never or on_demand".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    const MAX_BYTES: u64 = 1_048_576;

    #[must_use]
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            path: root.as_ref().join("settings.yaml"),
        }
    }

    /// Loads explicit settings or returns defaults without mutating disk.
    ///
    /// # Errors
    ///
    /// Returns an error if existing settings cannot be read or parsed.
    pub fn load(&self) -> Result<Settings, StoreError> {
        if let Some(parent) = self.path.parent() {
            match fs::symlink_metadata(parent) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(StoreError::InvalidSettings(
                        "settings root must be a real directory".to_owned(),
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Settings::default());
                }
                Err(error) => return Err(error.into()),
            }
        }
        match fs::symlink_metadata(&self.path) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || metadata.len() > Self::MAX_BYTES =>
            {
                return Err(StoreError::InvalidSettings(
                    "settings must be a regular file no larger than 1 MiB".to_owned(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Settings::default());
            }
            Err(error) => return Err(error.into()),
        }
        match fs::read_to_string(&self.path) {
            Ok(content) => {
                let settings: Settings = serde_yaml::from_str(&content)?;
                settings.validate()?;
                Ok(settings)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Atomically replaces explicit settings and synchronizes them to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization, temporary write, sync, or rename fails.
    pub fn save(&self, settings: &Settings) -> Result<(), StoreError> {
        settings.validate()?;
        if let Some(parent) = self.path.parent() {
            super::secure_directory(parent)?;
        }
        let bytes = serde_yaml::to_string(settings)?;
        write_bytes_atomic(&self.path, bytes.as_bytes(), true)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn passive_default_read_does_not_create_state() {
        let root = tempdir().unwrap();
        let store = SettingsStore::new(root.path());
        assert_eq!(store.load().unwrap(), Settings::default());
        assert!(!root.path().join("settings.yaml").exists());
    }

    #[test]
    fn explicit_save_round_trips() {
        let root = tempdir().unwrap();
        let store = SettingsStore::new(root.path());
        let settings = Settings {
            relays: vec!["wss://example.com".to_owned()],
            inbox_relays: vec!["wss://inbox.example.com".to_owned()],
            replication_threshold: 1,
            ..Settings::default()
        };
        store.save(&settings).unwrap();
        assert_eq!(store.load().unwrap(), settings);
        assert!(!root.path().join("settings.yaml.new").exists());
    }

    #[test]
    fn older_settings_enable_book_club_cross_links_by_default() {
        let root = tempdir().unwrap();
        let store = SettingsStore::new(root.path());
        let serialized = serde_yaml::to_string(&Settings::default()).unwrap();
        let legacy = serialized
            .lines()
            .filter(|line| {
                !line.starts_with("cross_links:")
                    && !line.trim_start().starts_with("book_club_enabled:")
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(root.path().join("settings.yaml"), legacy).unwrap();

        assert!(store.load().unwrap().cross_links.book_club_enabled);
    }

    #[test]
    fn appearance_defaults_are_light_stone_blue_and_migrate_older_settings() {
        let defaults = Settings::default();
        assert_eq!(defaults.theme, "light");
        assert_eq!(defaults.accent, "stone-blue");

        let root = tempdir().unwrap();
        let serialized = serde_yaml::to_string(&defaults).unwrap();
        let legacy = serialized
            .lines()
            .filter(|line| !line.starts_with("accent:"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(root.path().join("settings.yaml"), legacy).unwrap();

        assert_eq!(
            SettingsStore::new(root.path()).load().unwrap().accent,
            "stone-blue"
        );
    }

    #[cfg(unix)]
    #[test]
    fn settings_reject_symlink_reads_and_replace_symlinks_without_touching_targets() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let target = root.path().join("outside");
        fs::write(&target, "do not replace").unwrap();
        let path = root.path().join("settings.yaml");
        symlink(&target, &path).unwrap();
        let store = SettingsStore::new(root.path());
        assert!(store.load().is_err());

        let settings = Settings {
            relays: vec!["wss://example.com".to_owned()],
            inbox_relays: vec!["wss://inbox.example.com".to_owned()],
            replication_threshold: 1,
            ..Settings::default()
        };
        store.save(&settings).unwrap();
        assert_eq!(fs::read_to_string(target).unwrap(), "do not replace");
        assert!(!fs::symlink_metadata(path).unwrap().file_type().is_symlink());
        assert_eq!(store.load().unwrap(), settings);

        let outside = root.path().join("outside-directory");
        fs::create_dir(&outside).unwrap();
        let redirected = root.path().join("redirected");
        symlink(&outside, &redirected).unwrap();
        let redirected_store = SettingsStore::new(&redirected);
        assert!(redirected_store.load().is_err());
        assert!(redirected_store.save(&Settings::default()).is_err());
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    }

    #[test]
    fn oversized_settings_are_rejected_before_yaml_parsing() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("settings.yaml"),
            vec![b'a'; usize::try_from(SettingsStore::MAX_BYTES + 1).unwrap()],
        )
        .unwrap();
        assert!(matches!(
            SettingsStore::new(root.path()).load(),
            Err(StoreError::InvalidSettings(_))
        ));
    }

    #[test]
    fn persona_relay_preferences_override_defaults_without_leaking_between_personas() {
        let alice = hydra_domain::PersonaId::new();
        let bob = hydra_domain::PersonaId::new();
        let mut settings = Settings::default();
        settings.persona_relays.insert(
            alice.to_string(),
            PersonaRelaySettings {
                read: vec!["wss://alice-read.example".to_owned()],
                write: vec!["wss://alice-write.example".to_owned()],
            },
        );

        assert_eq!(
            settings.read_relays_for(alice),
            ["wss://alice-read.example"]
        );
        assert_eq!(
            settings.write_relays_for(alice),
            ["wss://alice-write.example"]
        );
        assert_eq!(settings.read_relays_for(bob), settings.relays);
        assert_eq!(settings.write_relays_for(bob), settings.relays);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("settings.yaml"),
            "relays: []\nreplication_threshold: 1\nshadow_policy: true\n",
        )
        .unwrap();
        assert!(SettingsStore::new(root.path()).load().is_err());
    }
}
