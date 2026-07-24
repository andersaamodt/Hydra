#![forbid(unsafe_code)]
//! Bounded, content-addressed media preservation and verification.

use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use hydra_domain::{AnchorId, DomainError, MediaManifest};
use hydra_store::StoreError;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

const MAX_BLOSSOM_DESCRIPTOR_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BlobDescriptor {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    #[serde(rename = "type")]
    pub mime_type: String,
    pub uploaded: u64,
}

#[derive(Debug, Clone)]
pub struct BlossomClient {
    client: reqwest::blocking::Client,
}

impl BlossomClient {
    /// Creates a bounded client that never follows server redirects during an
    /// authenticated upload.
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP client cannot be constructed.
    pub fn new() -> Result<Self, StoreError> {
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|error| StoreError::InvalidSettings(error.to_string()))?;
        Ok(Self { client })
    }

    /// Uploads exact bytes through Blossom BUD-02 with a narrowly scoped
    /// BUD-11 authorization token.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-HTTPS server, rejected upload, or descriptor
    /// that does not describe the supplied bytes exactly.
    pub fn upload(
        &self,
        server: &str,
        source: impl AsRef<Path>,
        manifest: &MediaManifest,
        authorization: &str,
    ) -> Result<BlobDescriptor, StoreError> {
        let mut endpoint =
            Url::parse(server).map_err(|error| StoreError::InvalidSettings(error.to_string()))?;
        if endpoint.scheme() != "https" || endpoint.host_str().is_none() {
            return Err(StoreError::InvalidSettings(
                "Blossom servers must use HTTPS".to_owned(),
            ));
        }
        endpoint.set_path("/upload");
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        let bytes = read_manifest_bytes(source.as_ref(), manifest)?;
        let response = self
            .client
            .put(endpoint)
            .header(reqwest::header::CONTENT_TYPE, &manifest.mime_type)
            .header("X-SHA-256", &manifest.sha256)
            .header(reqwest::header::AUTHORIZATION, authorization)
            .body(bytes)
            .send()
            .map_err(|error| StoreError::InvalidSettings(error.to_string()))?;
        if !matches!(response.status().as_u16(), 200 | 201) {
            let status = response.status();
            let reason = response
                .headers()
                .get("X-Reason")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("upload rejected")
                .to_owned();
            return Err(StoreError::InvalidSettings(format!(
                "Blossom {status}: {reason}"
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_BLOSSOM_DESCRIPTOR_BYTES)
        {
            return Err(StoreError::InvalidSettings(
                "Blossom descriptor exceeds Hydra's safe response limit".to_owned(),
            ));
        }
        let mut descriptor_bytes = Vec::new();
        response
            .take(MAX_BLOSSOM_DESCRIPTOR_BYTES + 1)
            .read_to_end(&mut descriptor_bytes)?;
        if descriptor_bytes.len() as u64 > MAX_BLOSSOM_DESCRIPTOR_BYTES {
            return Err(StoreError::InvalidSettings(
                "Blossom descriptor exceeds Hydra's safe response limit".to_owned(),
            ));
        }
        let descriptor = serde_json::from_slice::<BlobDescriptor>(&descriptor_bytes)
            .map_err(|error| StoreError::InvalidSettings(error.to_string()))?;
        let public_url = Url::parse(&descriptor.url)
            .map_err(|error| StoreError::InvalidSettings(error.to_string()))?;
        if public_url.scheme() != "https"
            || descriptor.sha256 != manifest.sha256
            || descriptor.size != manifest.size
        {
            return Err(StoreError::InvalidSettings(
                "Blossom descriptor does not match the preserved bytes".to_owned(),
            ));
        }
        Ok(descriptor)
    }
}

fn read_manifest_bytes(source: &Path, manifest: &MediaManifest) -> Result<Vec<u8>, StoreError> {
    manifest.validate()?;
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != manifest.size {
        return Err(StoreError::InvalidSettings(
            "upload source no longer matches the preserved media manifest".to_owned(),
        ));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(manifest.size)
            .map_err(|_| StoreError::InvalidSettings("media size is unsupported".to_owned()))?,
    );
    File::open(source)?
        .take(manifest.size + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != manifest.size
        || format!("{:x}", Sha256::digest(&bytes)) != manifest.sha256
    {
        return Err(StoreError::InvalidSettings(
            "upload source no longer matches the preserved media manifest".to_owned(),
        ));
    }
    Ok(bytes)
}

#[derive(Debug, Clone)]
pub struct MediaStore {
    root: PathBuf,
}

impl MediaStore {
    #[must_use]
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_owned(),
        }
    }

    /// Copies one bounded file into Hydra's content-addressed media directory.
    /// Existing content with the same hash is reused.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable/oversized input or failed durable I/O.
    pub fn preserve(
        &self,
        source: impl AsRef<Path>,
        object: AnchorId,
        mime_type: String,
        original_url: Option<String>,
        preserved_at: u64,
    ) -> Result<MediaManifest, StoreError> {
        self.preserve_with_limit(
            source,
            object,
            mime_type,
            original_url,
            preserved_at,
            MediaManifest::MAX_BYTES,
        )
    }

    /// Copies one file while enforcing the user's configured ceiling.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid limit, oversized input, or failed I/O.
    pub fn preserve_with_limit(
        &self,
        source: impl AsRef<Path>,
        object: AnchorId,
        mime_type: String,
        original_url: Option<String>,
        preserved_at: u64,
        max_bytes: u64,
    ) -> Result<MediaManifest, StoreError> {
        let source = source.as_ref();
        let size = fs::metadata(source)?.len();
        if max_bytes == 0 || max_bytes > MediaManifest::MAX_BYTES {
            return Err(StoreError::InvalidSettings(
                "media size ceiling is outside Hydra's safe range".to_owned(),
            ));
        }
        if size > max_bytes {
            return Err(StoreError::Domain(DomainError::TooLong {
                max: usize::try_from(max_bytes).unwrap_or(usize::MAX),
            }));
        }
        let mut input = File::open(source)?;
        let mut digest = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        let sha256 = format!("{:x}", digest.finalize());
        let dimensions = known_image_dimensions(source, &mime_type)?;
        let relative = PathBuf::from("media").join(&sha256);
        ensure_real_directory(&self.root)?;
        ensure_real_directory(&self.root.join("media"))?;
        let destination = self.root.join(&relative);
        if !destination.exists() {
            let directory = destination.parent().ok_or_else(|| {
                StoreError::InvalidSettings("media destination has no parent".to_owned())
            })?;
            fs::create_dir_all(directory)?;
            let temporary = tempfile::NamedTempFile::new_in(directory)?;
            fs::copy(source, temporary.path())?;
            temporary.as_file().sync_all()?;
            if let Err(error) = temporary.persist_noclobber(&destination)
                && error.error.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(error.error.into());
            }
            #[cfg(unix)]
            File::open(directory)?.sync_all()?;
        }
        let manifest = MediaManifest {
            object,
            sha256,
            mime_type,
            size,
            dimensions,
            duration_seconds: None,
            local_path: relative.to_string_lossy().into_owned(),
            original_url,
            blob_urls: Vec::new(),
            metadata_event_id: None,
            preserved_at,
        };
        manifest.validate()?;
        self.verify(&manifest)?;
        Ok(manifest)
    }

    /// Verifies that a preserved media file is present at its canonical path
    /// and still matches the recorded size and digest.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe path, missing data, size mismatch, or
    /// content mismatch.
    pub fn verify(&self, manifest: &MediaManifest) -> Result<(), StoreError> {
        manifest.validate()?;
        let expected = PathBuf::from("media").join(&manifest.sha256);
        if Path::new(&manifest.local_path) != expected {
            return Err(StoreError::InvalidSettings(
                "media manifest path is not content-addressed".to_owned(),
            ));
        }
        let path = self.root.join(expected);
        ensure_existing_real_directory(&self.root)?;
        ensure_existing_real_directory(&self.root.join("media"))?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StoreError::InvalidSettings(
                "preserved media must be a regular file".to_owned(),
            ));
        }
        if metadata.len() != manifest.size {
            return Err(StoreError::InvalidSettings(
                "preserved media size does not match manifest".to_owned(),
            ));
        }
        let mut input = File::open(path)?;
        let mut digest = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        if format!("{:x}", digest.finalize()) != manifest.sha256 {
            return Err(StoreError::InvalidSettings(
                "preserved media digest does not match manifest".to_owned(),
            ));
        }
        Ok(())
    }
}

fn ensure_real_directory(path: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            StoreError::InvalidSettings("media storage must use real directories".to_owned()),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            ensure_existing_real_directory(path)
        }
        Err(error) => Err(error.into()),
    }
}

fn ensure_existing_real_directory(path: &Path) -> Result<(), StoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StoreError::InvalidSettings(
            "media storage must use real directories".to_owned(),
        ));
    }
    Ok(())
}

fn known_image_dimensions(source: &Path, mime_type: &str) -> Result<Option<String>, StoreError> {
    if !mime_type.starts_with("image/") {
        return Ok(None);
    }
    let mut file = File::open(source)?;
    let mut bytes = Vec::new();
    file.by_ref().take(1024 * 1024).read_to_end(&mut bytes)?;
    let dimensions = match mime_type {
        "image/png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 => Some((
            u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
        )),
        "image/gif" if bytes.len() >= 10 && matches!(&bytes[..6], b"GIF87a" | b"GIF89a") => Some((
            u32::from(u16::from_le_bytes([bytes[6], bytes[7]])),
            u32::from(u16::from_le_bytes([bytes[8], bytes[9]])),
        )),
        "image/jpeg" => jpeg_dimensions(&bytes),
        _ => None,
    };
    Ok(dimensions
        .and_then(|(width, height)| (width > 0 && height > 0).then(|| format!("{width}x{height}"))))
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut offset = 2;
    while offset + 8 < bytes.len() {
        if bytes[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = bytes[offset + 1];
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            return Some((
                u32::from(u16::from_be_bytes([bytes[offset + 7], bytes[offset + 8]])),
                u32::from(u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]])),
            ));
        }
        if matches!(marker, 0xd8 | 0xd9) {
            offset += 2;
            continue;
        }
        let length = usize::from(u16::from_be_bytes([
            *bytes.get(offset + 2)?,
            *bytes.get(offset + 3)?,
        ]));
        if length < 2 {
            return None;
        }
        offset = offset.checked_add(length + 2)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use hydra_domain::{ContentBody, DurableEvent, NostrPublicKey, ObjectHead, ObjectKind};
    use hydra_store::DurableStore;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn media_is_content_addressed_deduplicated_verified_and_replayed() {
        let root = tempdir().unwrap();
        let source = root.path().join("source.png");
        fs::write(&source, b"not really a png, but stable test bytes").unwrap();
        let media = MediaStore::new(root.path());
        let anchor = AnchorId::parse("note1anchor").unwrap();
        let first = media
            .preserve(
                &source,
                anchor.clone(),
                "image/png".to_owned(),
                Some("https://example.test/source.png".to_owned()),
                20,
            )
            .unwrap();
        let second = media
            .preserve(&source, anchor.clone(), "image/png".to_owned(), None, 21)
            .unwrap();
        assert_eq!(first.sha256, second.sha256);

        let mut store = DurableStore::open(root.path()).unwrap();
        store
            .append(
                DurableEvent::NativeObjectChanged {
                    head: ObjectHead {
                        anchor: anchor.clone(),
                        author: NostrPublicKey::parse("npub-author").unwrap(),
                        kind: ObjectKind::Post,
                        title: Some("Title".to_owned()),
                        body: ContentBody::parse("body").unwrap(),
                        communities: vec![hydra_domain::CommunityKey::parse("science").unwrap()],
                        root: None,
                        parent: None,
                        external_root: None,
                        external_parent: None,
                        external_source: None,
                        edited_at: 10,
                    },
                    outbound: Vec::new(),
                },
                10,
            )
            .unwrap();
        store
            .append(DurableEvent::MediaPreserved(first.clone()), 20)
            .unwrap();
        drop(store);

        let reopened = DurableStore::open(root.path()).unwrap();
        assert_eq!(
            reopened.state().media.get(&(anchor, first.sha256.clone())),
            Some(&first)
        );
        media.verify(&first).unwrap();
        fs::write(root.path().join(&first.local_path), vec![b'x'; 39]).unwrap();
        assert!(media.verify(&first).is_err());
    }

    #[test]
    fn blossom_rejects_insecure_servers_before_network_access() {
        let root = tempdir().unwrap();
        let source = root.path().join("source.bin");
        fs::write(&source, b"bounded test bytes").unwrap();
        let manifest = MediaStore::new(root.path())
            .preserve(
                &source,
                AnchorId::parse("anchor").unwrap(),
                "application/octet-stream".to_owned(),
                None,
                1,
            )
            .unwrap();
        let error = BlossomClient::new()
            .unwrap()
            .upload("http://localhost:1234", source, &manifest, "Nostr token")
            .unwrap_err();
        assert!(error.to_string().contains("HTTPS"));
    }

    #[test]
    fn known_image_dimensions_are_recorded_without_decoding_active_content() {
        let root = tempdir().unwrap();
        let source = root.path().join("pixel.png");
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13_u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&320_u32.to_be_bytes());
        png.extend_from_slice(&240_u32.to_be_bytes());
        fs::write(&source, png).unwrap();
        let manifest = MediaStore::new(root.path())
            .preserve(
                source,
                AnchorId::parse("anchor").unwrap(),
                "image/png".to_owned(),
                None,
                1,
            )
            .unwrap();

        assert_eq!(manifest.dimensions.as_deref(), Some("320x240"));
    }

    #[cfg(unix)]
    #[test]
    fn media_paths_never_follow_hostile_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let source = root.path().join("source.bin");
        fs::write(&source, b"preserve these exact bytes").unwrap();
        let media = MediaStore::new(root.path().join("state"));
        let manifest = media
            .preserve(
                &source,
                AnchorId::parse("anchor").unwrap(),
                "application/octet-stream".to_owned(),
                None,
                1,
            )
            .unwrap();
        let target = root.path().join("outside");
        fs::write(&target, b"must remain untouched").unwrap();
        let preserved = root.path().join("state").join(&manifest.local_path);
        fs::remove_file(&preserved).unwrap();
        symlink(&target, &preserved).unwrap();

        assert!(media.verify(&manifest).is_err());
        assert!(
            media
                .preserve(
                    &source,
                    AnchorId::parse("anchor").unwrap(),
                    "application/octet-stream".to_owned(),
                    None,
                    2,
                )
                .is_err()
        );
        assert_eq!(fs::read(target).unwrap(), b"must remain untouched");

        let redirected_root = root.path().join("redirected");
        let outside_directory = root.path().join("outside-media");
        fs::create_dir(&outside_directory).unwrap();
        symlink(&outside_directory, &redirected_root).unwrap();
        assert!(
            MediaStore::new(&redirected_root)
                .preserve(
                    &source,
                    AnchorId::parse("other").unwrap(),
                    "application/octet-stream".to_owned(),
                    None,
                    3,
                )
                .is_err()
        );
        assert_eq!(fs::read_dir(outside_directory).unwrap().count(), 0);
    }
}
