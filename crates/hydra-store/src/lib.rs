#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    env, fmt,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
};

#[cfg(not(target_os = "macos"))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(test, not(target_os = "macos")))]
use std::{sync::mpsc, thread, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chacha20poly1305::{
    Key, XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, OsRng, Payload, rand_core::RngCore},
};
use fs2::FileExt;
use hydra_domain::{
    AnchorId, ArchiveId, ArchiveManifest, BlockRecord, CommunityKey, CommunitySubscription,
    ContinuityWorkflow, DeliveryState, DomainError, DurableEvent, EncryptedPrivateRecord,
    EventEnvelope, EventId, FollowRecord, MediaManifest, NostrPublicKey, ObjectHead, OperationId,
    OperationState, OutboundEvent, PersonaId, PersonaRegistry, Projection, ProjectionId,
    PublicFollowSet, PublicProjectionRecord, ReactionRecord, ReactionValue, RedditIdentityProof,
};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use thiserror::Error;

mod settings;

pub use settings::{
    CommunityAppearanceSetting, PersonaRelaySettings, ReadinessProbe, Settings, SettingsStore,
};

/// Runs a blocking platform-keyring operation without allowing a missing
/// desktop credential service to stall Hydra's local encrypted fallback.
///
/// macOS access stays synchronous so the user has time to answer a legitimate
/// Keychain authorization prompt. Other desktop sessions retain a short bound:
/// minimal Linux environments can otherwise wait roughly two minutes for D-Bus
/// activation before reporting that Secret Service is absent.
#[must_use]
pub fn try_platform_keyring<T, F>(operation: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    if env::var_os("HYDRA_DISABLE_KEYRING").is_some() {
        return None;
    }

    #[cfg(target_os = "macos")]
    {
        operation()
    }

    #[cfg(not(target_os = "macos"))]
    {
        static TIMED_OUT: AtomicBool = AtomicBool::new(false);
        if TIMED_OUT.load(Ordering::Relaxed) {
            return None;
        }
        let (result, timed_out) =
            try_platform_keyring_with_timeout(operation, Duration::from_secs(3));
        if timed_out {
            TIMED_OUT.store(true, Ordering::Relaxed);
        }
        result
    }
}

#[cfg(any(test, not(target_os = "macos")))]
fn try_platform_keyring_with_timeout<T, F>(operation: F, timeout: Duration) -> (Option<T>, bool)
where
    T: Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    if thread::Builder::new()
        .name("hydra-keyring".to_owned())
        .spawn(move || {
            let _ = sender.send(operation());
        })
        .is_err()
    {
        return (None, false);
    }
    match receiver.recv_timeout(timeout) {
        Ok(result) => (result, false),
        Err(mpsc::RecvTimeoutError::Timeout) => (None, true),
        Err(mpsc::RecvTimeoutError::Disconnected) => (None, false),
    }
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("object was not found")]
    NotFound,
    #[error("event log I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("event log contains invalid JSON at line {line}: {source}")]
    InvalidJson {
        line: usize,
        source: serde_json::Error,
    },
    #[error("event log checksum chain is invalid at line {line}")]
    InvalidChecksum { line: usize },
    #[error("event log record at line {line} exceeds {max} bytes")]
    RecordTooLarge { line: usize, max: usize },
    #[error("event serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error("settings data is invalid: {0}")]
    Settings(#[from] serde_yaml::Error),
    #[error("settings contract is invalid: {0}")]
    InvalidSettings(String),
    #[error("encrypted event storage failed: {0}")]
    Encryption(String),
    #[error("secure event-storage key is unavailable: {0}")]
    Credential(String),
}

/// Encrypted, permission-restricted credential storage for Linux desktops that
/// do not provide a usable Secret Service implementation.
///
/// Platform keyrings remain the preferred store. This vault is a deliberately
/// small local fallback so a gatekeeper-free Hydra persona can still be
/// created on minimal Linux installations. Its key and payloads never enter
/// the event log or repository.
#[derive(Debug, Clone)]
pub struct LocalCredentialVault {
    root: PathBuf,
    namespace: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialEnvelope {
    schema: String,
    nonce: String,
    ciphertext: String,
}

impl LocalCredentialVault {
    const SCHEMA: &'static str = "hydra-local-credential/v1";

    /// Opens one isolated credential namespace beneath Hydra's durable root.
    ///
    /// # Errors
    ///
    /// Returns an error when the namespace is unsafe or Hydra's root cannot be
    /// located.
    pub fn for_hydra(namespace: &str) -> Result<Self, StoreError> {
        if namespace.is_empty()
            || !namespace
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(StoreError::Credential(
                "local credential namespace is invalid".to_owned(),
            ));
        }
        let root = if let Some(root) = env::var_os("HYDRA_HOME") {
            PathBuf::from(root)
        } else {
            env::var_os("HOME")
                .map(|home| PathBuf::from(home).join("hydra"))
                .ok_or_else(|| {
                    StoreError::Credential(
                        "HOME is unavailable; set HYDRA_HOME explicitly".to_owned(),
                    )
                })?
        };
        Ok(Self {
            root,
            namespace: namespace.to_owned(),
        })
    }

    /// Encrypts and atomically stores one credential.
    ///
    /// # Errors
    ///
    /// Returns an error when the vault cannot be secured or written.
    pub fn set(&self, id: &str, secret: &str) -> Result<(), StoreError> {
        let key = load_or_create_credential_key(&self.root)?;
        let directory = self.directory();
        secure_directory(&directory)?;
        let mut nonce = [0_u8; 24];
        OsRng.fill_bytes(&mut nonce);
        let aad = self.aad(id);
        let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: secret.as_bytes(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| StoreError::Encryption("credential encryption failed".to_owned()))?;
        write_private_json_atomic(
            &self.path(id),
            &CredentialEnvelope {
                schema: Self::SCHEMA.to_owned(),
                nonce: BASE64.encode(nonce),
                ciphertext: BASE64.encode(ciphertext),
            },
        )
    }

    /// Decrypts one credential.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotFound`] when no local credential exists.
    pub fn get(&self, id: &str) -> Result<String, StoreError> {
        let path = self.path(id);
        if !path.exists() {
            return Err(StoreError::NotFound);
        }
        let envelope: CredentialEnvelope =
            serde_json::from_slice(&read_bounded_regular_file(&path, 64 * 1024)?)?;
        if envelope.schema != Self::SCHEMA {
            return Err(StoreError::Encryption(
                "unsupported local credential schema".to_owned(),
            ));
        }
        let nonce: [u8; 24] = BASE64
            .decode(envelope.nonce)
            .map_err(|error| StoreError::Encryption(error.to_string()))?
            .try_into()
            .map_err(|_| {
                StoreError::Encryption("credential nonce has the wrong length".to_owned())
            })?;
        let ciphertext = BASE64
            .decode(envelope.ciphertext)
            .map_err(|error| StoreError::Encryption(error.to_string()))?;
        let key = load_or_create_credential_key(&self.root)?;
        let aad = self.aad(id);
        let plaintext = XChaCha20Poly1305::new(Key::from_slice(&key))
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| StoreError::Encryption("credential authentication failed".to_owned()))?;
        String::from_utf8(plaintext).map_err(|error| StoreError::Encryption(error.to_string()))
    }

    /// Deletes one local fallback credential.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotFound`] when no local credential exists.
    pub fn delete(&self, id: &str) -> Result<(), StoreError> {
        let path = self.path(id);
        if !path.exists() {
            return Err(StoreError::NotFound);
        }
        fs::remove_file(path)?;
        Ok(())
    }

    fn directory(&self) -> PathBuf {
        self.root.join("credentials").join(&self.namespace)
    }

    fn path(&self, id: &str) -> PathBuf {
        let digest = Sha256::digest(id.as_bytes());
        self.directory().join(format!("{digest:x}.json"))
    }

    fn aad(&self, id: &str) -> String {
        format!("{}\0{id}", self.namespace)
    }
}

pub trait HeadStore {
    /// Appends one observed addressable-head version.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the version cannot be made durable.
    fn append_head(&mut self, head: ObjectHead) -> Result<(), StoreError>;

    /// Returns the newest observed head version for an anchor.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotFound`] when no version exists.
    fn current_head(&self, anchor: &AnchorId) -> Result<&ObjectHead, StoreError>;
    fn history(&self, anchor: &AnchorId) -> Vec<&ObjectHead>;
}

#[derive(Debug, Default, Clone)]
pub struct MemoryHeadStore {
    heads: BTreeMap<String, Vec<ObjectHead>>,
}

impl MemoryHeadStore {
    fn apply(&mut self, event: &DurableEvent) {
        if let DurableEvent::NativeObjectChanged { head, .. } = event {
            self.heads
                .entry(head.anchor.as_str().to_owned())
                .or_default()
                .push(head.clone());
        }
    }

    #[must_use]
    pub fn current_count(&self) -> usize {
        self.heads.len()
    }

    pub fn current_heads(&self) -> impl Iterator<Item = &ObjectHead> {
        self.heads
            .values()
            .filter_map(|history| history.iter().max_by_key(|head| head.edited_at))
    }

    #[must_use]
    pub fn contains(&self, anchor: &AnchorId) -> bool {
        self.heads.contains_key(anchor.as_str())
    }
}

impl HeadStore for MemoryHeadStore {
    fn append_head(&mut self, head: ObjectHead) -> Result<(), StoreError> {
        self.heads
            .entry(head.anchor.as_str().to_owned())
            .or_default()
            .push(head);
        Ok(())
    }

    fn current_head(&self, anchor: &AnchorId) -> Result<&ObjectHead, StoreError> {
        self.heads
            .get(anchor.as_str())
            .and_then(|history| history.iter().max_by_key(|head| head.edited_at))
            .ok_or(StoreError::NotFound)
    }

    fn history(&self, anchor: &AnchorId) -> Vec<&ObjectHead> {
        self.heads
            .get(anchor.as_str())
            .map_or_else(Vec::new, |heads| heads.iter().collect())
    }
}

pub struct EventLog {
    path: PathBuf,
    lock_path: PathBuf,
    key: [u8; 32],
}

impl fmt::Debug for EventLog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventLog")
            .field("path", &self.path)
            .field("lock_path", &self.lock_path)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct EncryptedEventRecord {
    schema: String,
    nonce: String,
    ciphertext: String,
}

impl EncryptedEventRecord {
    const SCHEMA: &'static str = "hydra-encrypted-event/v1";
}

#[derive(Deserialize)]
struct RawEventEnvelope {
    schema: String,
    id: EventId,
    recorded_at: u64,
    previous_checksum: Option<String>,
    event: Box<RawValue>,
    checksum: String,
}

struct StoredEvent {
    envelope: EventEnvelope,
    serialized: Option<Vec<u8>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoreKeyMode {
    Keyring,
    LocalFile,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoreKeyMetadata {
    schema: String,
    id: String,
    mode: StoreKeyMode,
}

impl EventLog {
    /// Opens or creates a checksummed append-only event log.
    ///
    /// # Errors
    ///
    /// Returns an error if the path cannot be created or existing records fail
    /// parsing or checksum validation.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = root.as_ref();
        secure_directory(root)?;
        let path = root.join("events.jsonl");
        let lock_path = root.join("events.lock");
        recover_interrupted_migration(&path)?;
        ensure_regular_file(&path)?;
        ensure_regular_file(&lock_path)?;
        let initialization_lock = OpenOptions::new().read(true).write(true).open(&lock_path)?;
        FileExt::lock_exclusive(&initialization_lock)?;
        let key = load_or_create_store_key(root)?;
        FileExt::unlock(&initialization_lock)?;
        let log = Self {
            path,
            lock_path,
            key,
        };
        let lock = log.exclusive_lock()?;
        let contains_legacy = Self::read_all_from(&log.path, &log.key, false)?.1;
        if contains_legacy {
            let events = Self::read_all_from(&log.path, &log.key, true)?.0;
            log.replace_unlocked(&events)?;
        }
        FileExt::unlock(&lock)?;
        Ok(log)
    }

    /// Appends one durable fact and synchronizes it to disk before returning.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization, append, or synchronization fails.
    pub fn append(&mut self, event: DurableEvent, recorded_at: u64) -> Result<EventId, StoreError> {
        event.validate()?;
        let lock = self.exclusive_lock()?;
        let (events, _) = Self::read_all_from(&self.path, &self.key, false)?;
        let id = self.append_unlocked(
            event,
            recorded_at,
            events.last().map(|stored| &stored.envelope),
        )?;
        FileExt::unlock(&lock)?;
        Ok(id)
    }

    /// Reads and verifies the complete event chain.
    ///
    /// # Errors
    ///
    /// Returns an error for I/O failure, malformed records, partial trailing
    /// records, or any checksum-chain break.
    pub fn read_all(&self) -> Result<Vec<EventEnvelope>, StoreError> {
        let lock = self.shared_lock()?;
        let (events, _) = Self::read_all_from(&self.path, &self.key, false)?;
        FileExt::unlock(&lock)?;
        Ok(events.into_iter().map(|stored| stored.envelope).collect())
    }

    fn read_all_from(
        path: &Path,
        key: &[u8; 32],
        preserve_serialized: bool,
    ) -> Result<(Vec<StoredEvent>, bool), StoreError> {
        const MAX_RECORD_BYTES: usize = 4 * 1024 * 1024;
        let mut reader = BufReader::new(File::open(path)?);
        let mut result = Vec::new();
        let mut previous: Option<String> = None;
        let mut contains_legacy = false;
        let mut line_number = 0;
        loop {
            let mut line = Vec::new();
            let read = reader
                .by_ref()
                .take(u64::try_from(MAX_RECORD_BYTES + 1).expect("fixed limit fits u64"))
                .read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            line_number += 1;
            if line.len() > MAX_RECORD_BYTES {
                return Err(StoreError::RecordTooLarge {
                    line: line_number,
                    max: MAX_RECORD_BYTES,
                });
            }
            while matches!(line.last(), Some(b'\n' | b'\r')) {
                line.pop();
            }
            let serialized = match serde_json::from_slice::<EncryptedEventRecord>(&line) {
                Ok(record) if record.schema == EncryptedEventRecord::SCHEMA => {
                    decrypt_event(&record, key).map_err(|message| {
                        StoreError::Encryption(format!("line {line_number}: {message}"))
                    })?
                }
                _ => {
                    contains_legacy = true;
                    line
                }
            };
            let raw: RawEventEnvelope =
                serde_json::from_slice(&serialized).map_err(|source| StoreError::InvalidJson {
                    line: line_number,
                    source,
                })?;
            let envelope: EventEnvelope =
                serde_json::from_slice(&serialized).map_err(|source| StoreError::InvalidJson {
                    line: line_number,
                    source,
                })?;
            let canonical_checksum = checksum_for(
                &raw.schema,
                raw.id,
                raw.recorded_at,
                raw.previous_checksum.as_deref(),
                &envelope.event,
            )?;
            let checksum_matches = raw.checksum == canonical_checksum
                || raw.checksum
                    == checksum_for_raw_event(
                        &raw.schema,
                        raw.id,
                        raw.recorded_at,
                        raw.previous_checksum.as_deref(),
                        &raw.event,
                    )?;
            if raw.schema != EventEnvelope::SCHEMA
                || raw.previous_checksum != previous
                || !checksum_matches
            {
                return Err(StoreError::InvalidChecksum { line: line_number });
            }
            previous = Some(raw.checksum);
            result.push(StoredEvent {
                envelope,
                serialized: preserve_serialized.then_some(serialized),
            });
        }
        Ok((result, contains_legacy))
    }

    /// Replays current materialized views from the durable arche.
    ///
    /// # Errors
    ///
    /// Returns an error if the log is invalid or persona invariants fail.
    pub fn replay(&self) -> Result<ReplayState, StoreError> {
        let mut state = ReplayState::default();
        for envelope in self.read_all()? {
            state.apply(&envelope.event, envelope.recorded_at)?;
        }
        Ok(state)
    }

    fn shared_lock(&self) -> Result<File, StoreError> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.lock_path)?;
        FileExt::lock_shared(&lock)?;
        Ok(lock)
    }

    fn exclusive_lock(&self) -> Result<File, StoreError> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.lock_path)?;
        FileExt::lock_exclusive(&lock)?;
        Ok(lock)
    }

    fn append_unlocked(
        &self,
        event: DurableEvent,
        recorded_at: u64,
        previous: Option<&EventEnvelope>,
    ) -> Result<EventId, StoreError> {
        let id = EventId::new();
        let previous_checksum = previous.map(|item| item.checksum.clone());
        let checksum = checksum_for(
            EventEnvelope::SCHEMA,
            id,
            recorded_at,
            previous_checksum.as_deref(),
            &event,
        )?;
        let envelope = EventEnvelope {
            schema: EventEnvelope::SCHEMA.to_owned(),
            id,
            recorded_at,
            previous_checksum,
            event,
            checksum,
        };
        let mut file = OpenOptions::new().append(true).open(&self.path)?;
        let encrypted = encrypt_event(&envelope, &self.key)?;
        serde_json::to_writer(&mut file, &encrypted)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(id)
    }

    fn replace_unlocked(&self, events: &[StoredEvent]) -> Result<(), StoreError> {
        let backup = self.path.with_extension("jsonl.migration-backup");
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        for stored in events {
            let serialized = stored.serialized.as_deref().ok_or_else(|| {
                StoreError::Io(std::io::Error::other(
                    "verified event bytes are unavailable for migration",
                ))
            })?;
            serde_json::to_writer(
                &mut temporary,
                &encrypt_serialized_event(serialized, &self.key)?,
            )?;
            temporary.write_all(b"\n")?;
        }
        temporary.as_file().sync_all()?;
        if fs::symlink_metadata(&backup).is_ok() {
            fs::remove_file(&backup)?;
        }
        fs::rename(&self.path, &backup)?;
        if let Err(error) = temporary.persist(&self.path) {
            let _ = fs::rename(&backup, &self.path);
            return Err(error.error.into());
        }
        fs::remove_file(backup)?;
        sync_directory(parent)?;
        Ok(())
    }
}

fn encrypt_event(
    envelope: &EventEnvelope,
    key: &[u8; 32],
) -> Result<EncryptedEventRecord, StoreError> {
    encrypt_serialized_event(&serde_json::to_vec(envelope)?, key)
}

fn encrypt_serialized_event(
    plaintext: &[u8],
    key: &[u8; 32],
) -> Result<EncryptedEventRecord, StoreError> {
    let mut nonce = [0_u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), plaintext)
        .map_err(|error| StoreError::Encryption(error.to_string()))?;
    Ok(EncryptedEventRecord {
        schema: EncryptedEventRecord::SCHEMA.to_owned(),
        nonce: BASE64.encode(nonce),
        ciphertext: BASE64.encode(ciphertext),
    })
}

fn decrypt_event(record: &EncryptedEventRecord, key: &[u8; 32]) -> Result<Vec<u8>, String> {
    let nonce = BASE64
        .decode(&record.nonce)
        .map_err(|error| error.to_string())?;
    let nonce: [u8; 24] = nonce
        .try_into()
        .map_err(|_| "encrypted event nonce has the wrong length".to_owned())?;
    let ciphertext = BASE64
        .decode(&record.ciphertext)
        .map_err(|error| error.to_string())?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(XNonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| "event authentication failed".to_owned())
}

fn load_or_create_store_key(root: &Path) -> Result<[u8; 32], StoreError> {
    const METADATA_SCHEMA: &str = "hydra-store-key/v1";
    const SERVICE: &str = "org.hydra.desktop.store";
    let metadata_path = root.join("store-key.json");
    if metadata_path.exists() {
        reject_symlink(&metadata_path)?;
        let metadata: StoreKeyMetadata =
            serde_json::from_slice(&read_bounded_regular_file(&metadata_path, 16 * 1024)?)?;
        if metadata.schema != METADATA_SCHEMA {
            return Err(StoreError::Encryption(
                "unsupported event-storage key metadata".to_owned(),
            ));
        }
        return match metadata.mode {
            StoreKeyMode::Keyring => {
                let id = metadata.id.clone();
                let encoded = try_platform_keyring(move || {
                    keyring::Entry::new(SERVICE, &id).ok()?.get_password().ok()
                })
                .ok_or_else(|| {
                    StoreError::Credential("platform keyring is unavailable".to_owned())
                })?;
                let key = decode_store_key(&encoded)?;
                write_private_key_file(root, &encoded)?;
                write_json_atomic(
                    &metadata_path,
                    &StoreKeyMetadata {
                        schema: METADATA_SCHEMA.to_owned(),
                        id: metadata.id,
                        mode: StoreKeyMode::LocalFile,
                    },
                )?;
                Ok(key)
            }
            StoreKeyMode::LocalFile => {
                let path = root.join(".store-key");
                reject_symlink(&path)?;
                let encoded = String::from_utf8(read_bounded_regular_file(&path, 1024)?)
                    .map_err(|error| StoreError::Encryption(error.to_string()))?;
                decode_store_key(encoded.trim())
            }
        };
    }

    let local_key_path = root.join(".store-key");
    if local_key_path.exists() {
        reject_symlink(&local_key_path)?;
        let encoded = String::from_utf8(read_bounded_regular_file(&local_key_path, 1024)?)
            .map_err(|error| StoreError::Encryption(error.to_string()))?;
        let key = decode_store_key(encoded.trim())?;
        write_json_atomic(
            &metadata_path,
            &StoreKeyMetadata {
                schema: METADATA_SCHEMA.to_owned(),
                id: uuid::Uuid::new_v4().to_string(),
                mode: StoreKeyMode::LocalFile,
            },
        )?;
        return Ok(key);
    }

    let id = uuid::Uuid::new_v4().to_string();
    let mut key = [0_u8; 32];
    OsRng.fill_bytes(&mut key);
    let encoded = BASE64.encode(key);
    write_private_key_file(root, &encoded)?;
    write_json_atomic(
        &metadata_path,
        &StoreKeyMetadata {
            schema: METADATA_SCHEMA.to_owned(),
            id,
            mode: StoreKeyMode::LocalFile,
        },
    )?;
    Ok(key)
}

fn decode_store_key(encoded: &str) -> Result<[u8; 32], StoreError> {
    BASE64
        .decode(encoded)
        .map_err(|error| StoreError::Encryption(error.to_string()))?
        .try_into()
        .map_err(|_| StoreError::Encryption("event-storage key has the wrong length".to_owned()))
}

fn write_private_key_file(root: &Path, encoded: &str) -> Result<(), StoreError> {
    let path = root.join(".store-key");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            file.write_all(encoded.as_bytes())?;
            file.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            reject_symlink(&path)?;
            let existing = String::from_utf8(read_bounded_regular_file(&path, 1024)?)
                .map_err(|error| StoreError::Encryption(error.to_string()))?;
            if existing.trim() == encoded {
                Ok(())
            } else {
                Err(StoreError::Credential(
                    "local event-storage key conflicts with Keychain".to_owned(),
                ))
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn load_or_create_credential_key(root: &Path) -> Result<[u8; 32], StoreError> {
    secure_directory(root)?;
    let path = root.join(".credential-key");
    if path.exists() {
        reject_symlink(&path)?;
        let encoded = String::from_utf8(read_bounded_regular_file(&path, 1024)?)
            .map_err(|error| StoreError::Encryption(error.to_string()))?;
        return decode_store_key(encoded.trim());
    }

    let mut key = [0_u8; 32];
    OsRng.fill_bytes(&mut key);
    let encoded = BASE64.encode(key);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            file.write_all(encoded.as_bytes())?;
            file.sync_all()?;
            Ok(key)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let encoded = String::from_utf8(read_bounded_regular_file(&path, 1024)?)
                .map_err(|error| StoreError::Encryption(error.to_string()))?;
            decode_store_key(encoded.trim())
        }
        Err(error) => Err(error.into()),
    }
}

fn secure_directory(path: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Hydra state path must be a real directory, not a symlink",
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn ensure_regular_file(path: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Hydra state file must be a regular file",
            )))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            match options.open(path) {
                Ok(file) => {
                    file.sync_all()?;
                    Ok(())
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let metadata = fs::symlink_metadata(path)?;
                    if metadata.file_type().is_symlink() || !metadata.is_file() {
                        Err(StoreError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "Hydra state file must be a regular file",
                        )))
                    } else {
                        Ok(())
                    }
                }
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn reject_symlink(path: &Path) -> Result<(), StoreError> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Hydra state file must not be a symlink",
        )));
    }
    Ok(())
}

fn read_bounded_regular_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, StoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        return Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Hydra state file is not a bounded regular file",
        )));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .map_err(|_| std::io::Error::other("state file size is unsupported"))?,
    );
    File::open(path)?
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Hydra state file exceeds its safety limit",
        )));
    }
    Ok(bytes)
}

fn write_private_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), StoreError> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    write_bytes_atomic(path, &bytes, true)
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), StoreError> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    write_bytes_atomic(path, &bytes, false)
}

pub(crate) fn write_bytes_atomic(
    path: &Path,
    bytes: &[u8],
    private: bool,
) -> Result<(), StoreError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt as _;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| StoreError::Io(error.error))?;
    sync_directory(parent)
}

fn recover_interrupted_migration(path: &Path) -> Result<(), StoreError> {
    let backup = path.with_extension("jsonl.migration-backup");
    let temporary = path.with_extension("jsonl.migrating");
    if !path.exists() && backup.exists() {
        fs::rename(&backup, path)?;
    }
    if temporary.exists() {
        fs::remove_file(temporary)?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[derive(Debug, Default, Clone)]
pub struct ReplayState {
    pub heads: MemoryHeadStore,
    pub personas: PersonaRegistry,
    pub inbox_relays: BTreeMap<PersonaId, Vec<String>>,
    pub persona_relays: BTreeMap<PersonaId, (Vec<String>, Vec<String>)>,
    pub reddit_identity_proofs: BTreeMap<PersonaId, RedditIdentityProof>,
    pub operations: BTreeMap<OperationId, OperationState>,
    pub continuity_workflows: BTreeMap<OperationId, ContinuityWorkflow>,
    pub outbound: BTreeMap<String, OutboundEvent>,
    pub deliveries: BTreeMap<(String, String), DeliveryState>,
    pub reactions: Vec<ReactionRecord>,
    pub private_records: Vec<EncryptedPrivateRecord>,
    pub archive_manifests: BTreeMap<ArchiveId, ArchiveManifest>,
    pub projections: BTreeMap<ProjectionId, Projection>,
    pub public_projections: BTreeMap<String, PublicProjectionRecord>,
    pub follows: BTreeMap<(PersonaId, NostrPublicKey), FollowRecord>,
    pub public_follow_sets: BTreeMap<(PersonaId, String), PublicFollowSet>,
    pub disowning_requests: BTreeMap<(PersonaId, AnchorId), String>,
    pub blocks: BTreeMap<(PersonaId, NostrPublicKey), BlockRecord>,
    pub subscriptions: BTreeMap<(PersonaId, CommunityKey), CommunitySubscription>,
    pub received_events: BTreeMap<String, String>,
    pub received_event_first_seen: BTreeMap<String, u64>,
    pub head_first_seen: BTreeMap<(AnchorId, u64), u64>,
    pub flocking_judgments: Vec<flocking_core::Judgment>,
    pub community_appearances: Vec<flocking_core::CommunityAppearance>,
    pub community_color_choices: Vec<hydra_domain::CommunityColorChoice>,
    pub media: BTreeMap<(AnchorId, String), MediaManifest>,
}

impl ReplayState {
    #[must_use]
    pub fn pending_delivery_count(&self) -> usize {
        self.outbound
            .values()
            .map(|event| {
                event
                    .relays
                    .iter()
                    .filter(|relay| {
                        !matches!(
                            self.deliveries
                                .get(&(event.event_id.clone(), (*relay).clone())),
                            Some(DeliveryState::Accepted)
                        )
                    })
                    .count()
            })
            .sum()
    }

    #[must_use]
    pub fn current_stance(
        &self,
        actor: &NostrPublicKey,
        target: &AnchorId,
    ) -> Option<&ReactionValue> {
        self.reactions
            .iter()
            .rev()
            .find(|reaction| {
                reaction.actor == *actor && reaction.target == *target && reaction.value.is_stance()
            })
            .map(|reaction| &reaction.value)
    }

    #[must_use]
    pub fn first_seen(&self, head: &ObjectHead) -> Option<u64> {
        self.head_first_seen
            .get(&(head.anchor.clone(), head.edited_at))
            .copied()
    }
}

impl ReplayState {
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive dispatcher keeps every durable event visibly replayable"
    )]
    fn apply(&mut self, event: &DurableEvent, recorded_at: u64) -> Result<(), DomainError> {
        event.validate()?;
        match event {
            DurableEvent::PersonaCreated(persona) => self.personas.insert(persona.clone()),
            DurableEvent::PersonaUpdated(persona) => self.personas.replace(persona.clone()),
            DurableEvent::PersonaProfilePublished {
                persona,
                display_name,
                outbound,
            } => self.apply_persona_profile(*persona, display_name, outbound),
            DurableEvent::RedditIdentityProofPublished { proof, outbound } => {
                self.apply_reddit_identity_proof(proof, outbound)
            }
            DurableEvent::InboxRelaysChanged {
                persona,
                relays,
                outbound,
            } => self.apply_inbox_relays(*persona, relays, outbound),
            DurableEvent::PersonaRelaysChanged {
                persona,
                read,
                write,
                outbound,
            } => self.apply_persona_relays(*persona, read, write, outbound),
            DurableEvent::OperationChanged {
                operation_id,
                state,
            } => self.apply_operation(*operation_id, *state),
            DurableEvent::ContinuityWorkflowChanged(workflow) => {
                self.apply_continuity_workflow(workflow)
            }
            DurableEvent::NativeObjectChanged { head, outbound, .. } => {
                self.heads.apply(event);
                self.head_first_seen
                    .entry((head.anchor.clone(), head.edited_at))
                    .or_insert(recorded_at);
                for item in outbound {
                    self.outbound.insert(item.event_id.clone(), item.clone());
                }
                Ok(())
            }
            DurableEvent::PublicEventQueued { persona, outbound } => {
                if !self.personas.contains(*persona) {
                    return Err(DomainError::MissingPersona);
                }
                self.outbound
                    .insert(outbound.event_id.clone(), outbound.clone());
                Ok(())
            }
            DurableEvent::ObjectDisowningRequested {
                persona,
                anchor,
                reason,
                outbound,
            } => self.apply_disowning_request(*persona, anchor, reason, outbound),
            DurableEvent::RemoteEventReceived {
                event_id,
                event_json,
                heads,
                reactions,
                public_projections,
                flocking_judgments,
                community_appearances,
                community_color_choices,
            } => self.apply_remote_event(
                event_id,
                event_json,
                heads,
                reactions,
                public_projections,
                flocking_judgments,
                community_appearances,
                community_color_choices,
                recorded_at,
            ),
            DurableEvent::MediaPreserved(manifest)
            | DurableEvent::MediaPreservedFor { manifest, .. } => self.apply_media(manifest),
            DurableEvent::MediaPublished {
                manifest, outbound, ..
            } => self.apply_published_media(manifest, outbound),
            DurableEvent::DeliveryRecorded {
                event_id,
                relay,
                state,
            } => self.apply_delivery(event_id, relay, state),
            DurableEvent::ReactionRecorded { reaction, outbound } => {
                self.apply_reaction(reaction, outbound)
            }
            DurableEvent::PrivateRecordStored {
                record,
                public_follow_retraction,
                public_block_retraction,
                public_subscription_retraction,
                outbound,
            } => self.apply_private_record(
                record,
                public_follow_retraction.as_ref(),
                public_block_retraction.as_ref(),
                public_subscription_retraction.as_ref(),
                outbound,
            ),
            DurableEvent::FollowChanged { follow, outbound } => self.apply_follow(follow, outbound),
            DurableEvent::PublicFollowSetPublished { set, outbound } => {
                self.apply_public_follow_set(set, outbound)
            }
            DurableEvent::CommunitySubscriptionChanged {
                subscription,
                outbound,
            } => self.apply_subscription(subscription, outbound),
            DurableEvent::BlockChanged { block, outbound } => self.apply_block(block, outbound),
            DurableEvent::FlockingJudgmentChanged { record, outbound } => {
                self.apply_flocking_judgment(record, outbound)
            }
            DurableEvent::ArchiveCaptured(manifest) => self.apply_archive_manifest(manifest),
            DurableEvent::ProjectionChanged {
                projection,
                outbound,
            } => self.apply_projection(projection, outbound.as_ref()),
        }
    }

    fn apply_continuity_workflow(
        &mut self,
        workflow: &ContinuityWorkflow,
    ) -> Result<(), DomainError> {
        if let Some(current) = self.continuity_workflows.get(&workflow.id) {
            if current.persona != workflow.persona
                || !current.state.can_transition_to(workflow.state)
            {
                return Err(DomainError::InvalidTransition {
                    from: format!("{:?}", current.state),
                    to: format!("{:?}", workflow.state),
                });
            }
        } else if !workflow.state.is_initial() {
            return Err(DomainError::InvalidTransition {
                from: "Absent".to_owned(),
                to: format!("{:?}", workflow.state),
            });
        }
        self.continuity_workflows
            .insert(workflow.id, workflow.clone());
        Ok(())
    }

    fn apply_archive_manifest(&mut self, manifest: &ArchiveManifest) -> Result<(), DomainError> {
        if !self.personas.contains(manifest.observer) {
            return Err(DomainError::MissingPersona);
        }
        manifest.validate()?;
        self.archive_manifests.insert(manifest.id, manifest.clone());
        Ok(())
    }

    fn apply_published_media(
        &mut self,
        manifest: &MediaManifest,
        outbound: &OutboundEvent,
    ) -> Result<(), DomainError> {
        self.apply_media(manifest)?;
        self.outbound
            .insert(outbound.event_id.clone(), outbound.clone());
        Ok(())
    }

    fn apply_delivery(
        &mut self,
        event_id: &str,
        relay: &str,
        state: &DeliveryState,
    ) -> Result<(), DomainError> {
        if !self.outbound.contains_key(event_id) {
            return Err(DomainError::UnknownOutboundEvent);
        }
        self.deliveries
            .insert((event_id.to_owned(), relay.to_owned()), state.clone());
        Ok(())
    }

    fn apply_operation(
        &mut self,
        operation_id: OperationId,
        state: OperationState,
    ) -> Result<(), DomainError> {
        if let Some(current) = self.operations.get(&operation_id) {
            current.transition(state)?;
        } else if state != OperationState::Queued {
            return Err(DomainError::InvalidTransition {
                from: "Absent".to_owned(),
                to: format!("{state:?}"),
            });
        }
        self.operations.insert(operation_id, state);
        Ok(())
    }

    fn apply_persona_profile(
        &mut self,
        persona: PersonaId,
        display_name: &str,
        outbound: &OutboundEvent,
    ) -> Result<(), DomainError> {
        if display_name.trim().is_empty() {
            return Err(DomainError::Empty);
        }
        let mut current = self
            .personas
            .get(persona)
            .cloned()
            .ok_or(DomainError::MissingPersona)?;
        display_name.clone_into(&mut current.display_name);
        self.personas.replace(current)?;
        self.outbound
            .insert(outbound.event_id.clone(), outbound.clone());
        Ok(())
    }

    fn apply_reddit_identity_proof(
        &mut self,
        proof: &RedditIdentityProof,
        outbound: &OutboundEvent,
    ) -> Result<(), DomainError> {
        proof.validate()?;
        if !self.personas.contains(proof.persona) {
            return Err(DomainError::MissingPersona);
        }
        self.reddit_identity_proofs
            .insert(proof.persona, proof.clone());
        self.outbound
            .insert(outbound.event_id.clone(), outbound.clone());
        Ok(())
    }

    fn apply_inbox_relays(
        &mut self,
        persona: PersonaId,
        relays: &[String],
        outbound: &OutboundEvent,
    ) -> Result<(), DomainError> {
        if !self.personas.contains(persona) {
            return Err(DomainError::MissingPersona);
        }
        if relays.is_empty() || relays.iter().any(String::is_empty) {
            return Err(DomainError::Empty);
        }
        self.inbox_relays.insert(persona, relays.to_vec());
        self.outbound
            .insert(outbound.event_id.clone(), outbound.clone());
        Ok(())
    }

    fn apply_persona_relays(
        &mut self,
        persona: PersonaId,
        read: &[String],
        write: &[String],
        outbound: &OutboundEvent,
    ) -> Result<(), DomainError> {
        if !self.personas.contains(persona) {
            return Err(DomainError::MissingPersona);
        }
        if read.is_empty() || write.is_empty() || read.iter().chain(write).any(String::is_empty) {
            return Err(DomainError::Empty);
        }
        self.persona_relays
            .insert(persona, (read.to_vec(), write.to_vec()));
        self.outbound
            .insert(outbound.event_id.clone(), outbound.clone());
        Ok(())
    }

    fn apply_media(&mut self, manifest: &MediaManifest) -> Result<(), DomainError> {
        if !self.heads.contains(&manifest.object) {
            return Err(DomainError::MissingObject);
        }
        manifest.validate()?;
        self.media.insert(
            (manifest.object.clone(), manifest.sha256.clone()),
            manifest.clone(),
        );
        Ok(())
    }

    fn apply_projection(
        &mut self,
        projection: &Projection,
        outbound: Option<&OutboundEvent>,
    ) -> Result<(), DomainError> {
        if !self.personas.contains(projection.persona) {
            return Err(DomainError::MissingPersona);
        }
        if !self.heads.contains(&projection.anchor) {
            return Err(DomainError::MissingObject);
        }
        let current = self
            .projections
            .get(&projection.id)
            .map_or(hydra_domain::ProjectionState::NotRequested, |current| {
                current.state
            });
        if !current.can_transition_to(projection.state) {
            return Err(DomainError::InvalidTransition {
                from: format!("{current:?}"),
                to: format!("{:?}", projection.state),
            });
        }
        self.projections.insert(projection.id, projection.clone());
        if let Some(outbound) = outbound {
            self.outbound
                .insert(outbound.event_id.clone(), outbound.clone());
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the durable event fields stay explicit at the replay boundary"
    )]
    fn apply_remote_event(
        &mut self,
        event_id: &str,
        event_json: &str,
        heads: &[ObjectHead],
        reactions: &[ReactionRecord],
        public_projections: &[PublicProjectionRecord],
        flocking_judgments: &[flocking_core::Judgment],
        community_appearances: &[flocking_core::CommunityAppearance],
        community_color_choices: &[hydra_domain::CommunityColorChoice],
        recorded_at: u64,
    ) -> Result<(), DomainError> {
        if event_id.is_empty() || event_json.is_empty() {
            return Err(DomainError::Empty);
        }
        if self.received_events.contains_key(event_id) {
            return Ok(());
        }
        for head in heads {
            self.heads
                .append_head(head.clone())
                .map_err(|_| DomainError::MissingObject)?;
            self.head_first_seen
                .entry((head.anchor.clone(), head.edited_at))
                .or_insert(recorded_at);
        }
        for reaction in reactions {
            if !self.has_reaction_target(&reaction.target) {
                return Err(DomainError::MissingObject);
            }
            reaction.value.wire_value()?;
            if !self
                .reactions
                .iter()
                .any(|known| known.source_event_id == reaction.source_event_id)
            {
                self.reactions.push(reaction.clone());
            }
        }
        for projection in public_projections {
            projection.validate()?;
            self.public_projections
                .entry(projection.address.clone())
                .and_modify(|current| {
                    if projection.recorded_at > current.recorded_at
                        || (projection.recorded_at == current.recorded_at
                            && projection.source_event_id > current.source_event_id)
                    {
                        *current = projection.clone();
                    }
                })
                .or_insert_with(|| projection.clone());
        }
        self.flocking_judgments
            .extend(flocking_judgments.iter().cloned());
        self.community_appearances
            .extend(community_appearances.iter().cloned());
        self.community_color_choices
            .extend(community_color_choices.iter().cloned());
        self.received_events
            .insert(event_id.to_owned(), event_json.to_owned());
        self.received_event_first_seen
            .insert(event_id.to_owned(), recorded_at);
        Ok(())
    }

    fn apply_flocking_judgment(
        &mut self,
        record: &hydra_domain::FlockingJudgmentRecord,
        outbound: &[OutboundEvent],
    ) -> Result<(), DomainError> {
        if !self.personas.contains(record.persona) || !record.public {
            return Err(DomainError::InvalidFlocking);
        }
        record.validate()?;
        self.flocking_judgments.push(record.judgment.clone());
        self.apply_outbound(outbound);
        Ok(())
    }

    fn has_reaction_target(&self, target: &AnchorId) -> bool {
        self.heads.contains(target)
    }

    fn apply_private_record(
        &mut self,
        record: &EncryptedPrivateRecord,
        follow_retraction: Option<&FollowRecord>,
        block_retraction: Option<&BlockRecord>,
        subscription_retraction: Option<&CommunitySubscription>,
        outbound: &[OutboundEvent],
    ) -> Result<(), DomainError> {
        if !self.personas.contains(record.persona) {
            return Err(DomainError::MissingPersona);
        }
        if record.ciphertext.is_empty() {
            return Err(DomainError::Empty);
        }
        if let Some(follow) = follow_retraction {
            self.apply_follow(follow, &[])?;
        }
        if let Some(block) = block_retraction {
            self.apply_block(block, &[])?;
        }
        if let Some(subscription) = subscription_retraction {
            self.apply_subscription(subscription, &[])?;
        }
        self.private_records.push(record.clone());
        self.apply_outbound(outbound);
        Ok(())
    }

    fn apply_follow(
        &mut self,
        follow: &FollowRecord,
        outbound: &[OutboundEvent],
    ) -> Result<(), DomainError> {
        if !self.personas.contains(follow.persona) {
            return Err(DomainError::MissingPersona);
        }
        self.follows
            .insert((follow.persona, follow.target.clone()), follow.clone());
        self.apply_outbound(outbound);
        Ok(())
    }

    fn apply_public_follow_set(
        &mut self,
        set: &PublicFollowSet,
        outbound: &OutboundEvent,
    ) -> Result<(), DomainError> {
        if !self.personas.contains(set.persona) {
            return Err(DomainError::MissingPersona);
        }
        set.validate()?;
        self.public_follow_sets
            .insert((set.persona, set.identifier.clone()), set.clone());
        self.outbound
            .insert(outbound.event_id.clone(), outbound.clone());
        Ok(())
    }

    fn apply_disowning_request(
        &mut self,
        persona: PersonaId,
        anchor: &AnchorId,
        reason: &str,
        outbound: &OutboundEvent,
    ) -> Result<(), DomainError> {
        let owner = self
            .personas
            .get(persona)
            .ok_or(DomainError::MissingPersona)?;
        let head = self
            .heads
            .current_head(anchor)
            .map_err(|_| DomainError::MissingObject)?;
        if head.author != owner.public_key {
            return Err(DomainError::IdentityConflict);
        }
        if reason.len() > 500 {
            return Err(DomainError::TooLong { max: 500 });
        }
        self.disowning_requests
            .insert((persona, anchor.clone()), reason.to_owned());
        self.outbound
            .insert(outbound.event_id.clone(), outbound.clone());
        Ok(())
    }

    fn apply_reaction(
        &mut self,
        reaction: &ReactionRecord,
        outbound: &OutboundEvent,
    ) -> Result<(), DomainError> {
        if !self.has_reaction_target(&reaction.target) {
            return Err(DomainError::MissingObject);
        }
        reaction.value.wire_value()?;
        if reaction.credited_reaffirmation && !reaction.value.can_reaffirm() {
            return Err(DomainError::InvalidReaction);
        }
        self.reactions.push(reaction.clone());
        self.outbound
            .insert(outbound.event_id.clone(), outbound.clone());
        Ok(())
    }

    fn apply_block(
        &mut self,
        block: &BlockRecord,
        outbound: &[OutboundEvent],
    ) -> Result<(), DomainError> {
        if !self.personas.contains(block.persona) {
            return Err(DomainError::MissingPersona);
        }
        block.validate()?;
        self.blocks
            .insert((block.persona, block.target.clone()), block.clone());
        self.apply_outbound(outbound);
        Ok(())
    }

    fn apply_subscription(
        &mut self,
        subscription: &CommunitySubscription,
        outbound: &[OutboundEvent],
    ) -> Result<(), DomainError> {
        if !self.personas.contains(subscription.persona) {
            return Err(DomainError::MissingPersona);
        }
        self.subscriptions.insert(
            (subscription.persona, subscription.community.clone()),
            subscription.clone(),
        );
        self.apply_outbound(outbound);
        Ok(())
    }

    fn apply_outbound(&mut self, outbound: &[OutboundEvent]) {
        for event in outbound {
            self.outbound.insert(event.event_id.clone(), event.clone());
        }
    }
}

/// The validating production write path to Hydra's durable event arche.
#[derive(Debug)]
pub struct DurableStore {
    log: EventLog,
    state: ReplayState,
}

impl DurableStore {
    /// Opens and replays an existing durable root.
    ///
    /// # Errors
    ///
    /// Returns an error for I/O, checksum, parsing, or semantic replay failure.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let log = EventLog::open(root)?;
        let state = log.replay()?;
        Ok(Self { log, state })
    }

    /// Validates and commits one event without allowing invalid durable state.
    ///
    /// # Errors
    ///
    /// Returns before writing when the event violates domain invariants, or after
    /// an append/synchronization failure when durability cannot be established.
    pub fn append(&mut self, event: DurableEvent, recorded_at: u64) -> Result<EventId, StoreError> {
        let lock = self.log.exclusive_lock()?;
        let (envelopes, _) = EventLog::read_all_from(&self.log.path, &self.log.key, false)?;
        let mut candidate = ReplayState::default();
        for stored in &envelopes {
            candidate.apply(&stored.envelope.event, stored.envelope.recorded_at)?;
        }
        candidate.apply(&event, recorded_at)?;
        let id = self.log.append_unlocked(
            event,
            recorded_at,
            envelopes.last().map(|stored| &stored.envelope),
        )?;
        FileExt::unlock(&lock)?;
        self.state = candidate;
        Ok(id)
    }

    #[must_use]
    pub const fn state(&self) -> &ReplayState {
        &self.state
    }

    /// Returns the verified append-only evidence ledger in recorded order.
    ///
    /// # Errors
    ///
    /// Returns an error if the encrypted log cannot be read or its checksum
    /// chain no longer verifies.
    pub fn raw_events(&self) -> Result<Vec<EventEnvelope>, StoreError> {
        self.log.read_all()
    }
}

fn checksum_for(
    schema: &str,
    id: EventId,
    recorded_at: u64,
    previous_checksum: Option<&str>,
    event: &DurableEvent,
) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    struct Payload<'a> {
        schema: &'a str,
        id: EventId,
        recorded_at: u64,
        previous_checksum: Option<&'a str>,
        event: &'a DurableEvent,
    }
    let bytes = serde_json::to_vec(&Payload {
        schema,
        id,
        recorded_at,
        previous_checksum,
        event,
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn checksum_for_raw_event(
    schema: &str,
    id: EventId,
    recorded_at: u64,
    previous_checksum: Option<&str>,
    event: &RawValue,
) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    struct Payload<'a> {
        schema: &'a str,
        id: EventId,
        recorded_at: u64,
        previous_checksum: Option<&'a str>,
        event: &'a RawValue,
    }
    let bytes = serde_json::to_vec(&Payload {
        schema,
        id,
        recorded_at,
        previous_checksum,
        event,
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write as _,
        sync::{Arc, Barrier},
        thread,
        time::{Duration, Instant},
    };

    use hydra_domain::{
        CommunityKey, ContentBody, NostrPublicKey, ObjectKind, Persona, PersonaId, RedditAccountId,
    };
    use tempfile::tempdir;

    use super::*;

    fn head(body: &str, edited_at: u64) -> ObjectHead {
        ObjectHead {
            anchor: AnchorId::parse("note1anchor").unwrap(),
            author: NostrPublicKey::parse("npub-author").unwrap(),
            kind: ObjectKind::Post,
            title: Some("Title".to_owned()),
            body: ContentBody::parse(body).unwrap(),
            communities: vec![CommunityKey::parse("science").unwrap()],
            root: None,
            parent: None,
            external_root: None,
            external_parent: None,
            external_source: None,
            edited_at,
        }
    }

    #[test]
    fn replay_keeps_history_and_selects_latest_head() {
        let anchor = AnchorId::parse("note1anchor").unwrap();
        let initial = head("first", 10);
        let revised = initial.revised(ContentBody::parse("second").unwrap(), 20);
        let mut store = MemoryHeadStore::default();
        store.append_head(revised).unwrap();
        store.append_head(initial).unwrap();
        assert_eq!(store.current_head(&anchor).unwrap().body.as_str(), "second");
        assert_eq!(store.history(&anchor).len(), 2);
    }

    #[test]
    fn durable_log_reopens_and_replays_identically() {
        let root = tempdir().unwrap();
        let mut log = EventLog::open(root.path()).unwrap();
        log.append(
            DurableEvent::NativeObjectChanged {
                head: head("first", 10),
                outbound: Vec::new(),
            },
            10,
        )
        .unwrap();
        log.append(
            DurableEvent::NativeObjectChanged {
                head: head("second", 20),
                outbound: Vec::new(),
            },
            20,
        )
        .unwrap();
        drop(log);

        let reopened = EventLog::open(root.path()).unwrap();
        assert_eq!(reopened.read_all().unwrap().len(), 2);
        assert_eq!(
            reopened
                .replay()
                .unwrap()
                .heads
                .current_head(&AnchorId::parse("note1anchor").unwrap())
                .unwrap()
                .body
                .as_str(),
            "second"
        );
    }

    #[test]
    fn remote_head_first_seen_survives_reopen_and_replay() {
        let root = tempdir().unwrap();
        let remote = head("backdated remote revision", 7);
        let mut store = DurableStore::open(root.path()).unwrap();
        store
            .append(
                DurableEvent::RemoteEventReceived {
                    event_id: "1".repeat(64),
                    event_json: "{}".to_owned(),
                    heads: vec![remote.clone()],
                    reactions: Vec::new(),
                    public_projections: Vec::new(),
                    flocking_judgments: Vec::new(),
                    community_appearances: Vec::new(),
                    community_color_choices: Vec::new(),
                },
                42,
            )
            .unwrap();
        assert_eq!(store.state().first_seen(&remote), Some(42));
        drop(store);

        let reopened = DurableStore::open(root.path()).unwrap();
        assert_eq!(reopened.state().first_seen(&remote), Some(42));
        assert_eq!(
            reopened
                .state()
                .received_event_first_seen
                .get(&"1".repeat(64)),
            Some(&42)
        );
    }

    #[test]
    fn durable_log_encrypts_event_bodies_at_rest() {
        let root = tempdir().unwrap();
        let mut log = EventLog::open(root.path()).unwrap();
        log.append(
            DurableEvent::NativeObjectChanged {
                head: head("plaintext must not survive at rest", 10),
                outbound: Vec::new(),
            },
            10,
        )
        .unwrap();
        let bytes = fs::read(root.path().join("events.jsonl")).unwrap();
        let serialized = String::from_utf8(bytes).unwrap();
        assert!(serialized.contains(EncryptedEventRecord::SCHEMA));
        assert!(!serialized.contains("plaintext must not survive at rest"));
        assert_eq!(log.read_all().unwrap().len(), 1);
    }

    #[test]
    fn opening_a_legacy_log_migrates_it_without_losing_events() {
        let root = tempdir().unwrap();
        let event = DurableEvent::NativeObjectChanged {
            head: head("legacy visible body", 10),
            outbound: Vec::new(),
        };
        let id = EventId::new();
        let envelope = EventEnvelope {
            schema: EventEnvelope::SCHEMA.to_owned(),
            id,
            recorded_at: 10,
            previous_checksum: None,
            checksum: checksum_for(EventEnvelope::SCHEMA, id, 10, None, &event).unwrap(),
            event,
        };
        fs::write(
            root.path().join("events.jsonl"),
            format!("{}\n", serde_json::to_string(&envelope).unwrap()),
        )
        .unwrap();

        let log = EventLog::open(root.path()).unwrap();
        assert_eq!(log.read_all().unwrap(), vec![envelope]);
        let serialized = fs::read_to_string(root.path().join("events.jsonl")).unwrap();
        assert!(serialized.contains(EncryptedEventRecord::SCHEMA));
        assert!(!serialized.contains("legacy visible body"));
    }

    #[test]
    fn canonical_checksum_accepts_equivalent_json_formatting() {
        let root = tempdir().unwrap();
        let event = DurableEvent::PersonaCreated(Persona {
            id: PersonaId::parse("00000000-0000-4000-8000-000000000010").unwrap(),
            public_key: NostrPublicKey::parse("npub-author").unwrap(),
            display_name: "Alice".to_owned(),
            reddit_account: None,
        });
        let id = EventId::new();
        let checksum = checksum_for(EventEnvelope::SCHEMA, id, 10, None, &event).unwrap();
        let reformatted = serde_json::to_string(&serde_json::json!({
            "schema": EventEnvelope::SCHEMA,
            "id": id,
            "recorded_at": 10,
            "previous_checksum": null,
            "event": serde_json::to_value(&event).unwrap(),
            "checksum": checksum,
        }))
        .unwrap();
        let raw: RawEventEnvelope = serde_json::from_str(&reformatted).unwrap();
        assert_ne!(
            checksum_for_raw_event(
                &raw.schema,
                raw.id,
                raw.recorded_at,
                raw.previous_checksum.as_deref(),
                &raw.event,
            )
            .unwrap(),
            raw.checksum
        );
        fs::write(root.path().join("events.jsonl"), format!("{reformatted}\n")).unwrap();

        let events = EventLog::open(root.path()).unwrap().read_all().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].checksum, raw.checksum);
        assert_eq!(
            EventLog::open(root.path()).unwrap().read_all().unwrap(),
            events
        );
    }

    #[test]
    fn opening_a_pre_external_source_log_preserves_its_valid_checksum() {
        let root = tempdir().unwrap();
        let first_checksum = "88a01f1acf3fcb0a832e5b8ec5527f6c9ea15693a4fff9f5eb94d1cda9fc6876";
        let second_checksum = "533881cd0a925cd615017208f9a318f40cbaa87b8f9da026446ba45477dc2d8d";
        let log = concat!(
            r#"{"schema":"hydra-event/v1","id":"00000000-0000-4000-8000-000000000001","recorded_at":10,"previous_checksum":null,"event":{"type":"persona_created","data":{"id":"00000000-0000-4000-8000-000000000010","public_key":"npub-author","display_name":"Alice","reddit_account":null}},"checksum":"88a01f1acf3fcb0a832e5b8ec5527f6c9ea15693a4fff9f5eb94d1cda9fc6876"}"#,
            "\n",
            r#"{"schema":"hydra-event/v1","id":"00000000-0000-4000-8000-000000000002","recorded_at":11,"previous_checksum":"88a01f1acf3fcb0a832e5b8ec5527f6c9ea15693a4fff9f5eb94d1cda9fc6876","event":{"type":"native_object_changed","data":{"head":{"anchor":"note1anchor","author":"npub-author","kind":"post","title":"Title","body":"legacy body","communities":["science"],"root":null,"parent":null,"edited_at":11},"outbound":[]}},"checksum":"533881cd0a925cd615017208f9a318f40cbaa87b8f9da026446ba45477dc2d8d"}"#,
            "\n"
        );
        fs::write(root.path().join("events.jsonl"), log).unwrap();

        let log = EventLog::open(root.path()).unwrap();
        let events = log.read_all().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].checksum, first_checksum);
        assert_eq!(events[1].checksum, second_checksum);
        let DurableEvent::NativeObjectChanged { head, .. } = &events[1].event else {
            panic!("second fixture record should be a native object");
        };
        assert_eq!(head.external_root, None);
        assert_eq!(head.external_parent, None);
        assert_eq!(head.external_source, None);
        drop(log);

        let encrypted = fs::read_to_string(root.path().join("events.jsonl")).unwrap();
        assert!(encrypted.contains(EncryptedEventRecord::SCHEMA));
        assert!(!encrypted.contains("legacy body"));
        assert_eq!(
            EventLog::open(root.path()).unwrap().read_all().unwrap(),
            events
        );
    }

    #[test]
    fn a_tampered_pre_external_source_record_is_still_rejected() {
        let root = tempdir().unwrap();
        let line = concat!(
            r#"{"schema":"hydra-event/v1","id":"00000000-0000-0000-0000-000000000001","recorded_at":10,"previous_checksum":null,"event":{"type":"native_object_changed","data":{"head":{"anchor":"note1anchor","author":"npub-author","kind":"post","title":"Title","body":"tampered body","communities":["science"],"root":null,"parent":null,"edited_at":10},"outbound":[]}},"checksum":"f0c6e14b7303f9427db2a942ee307e673c0a2e2c113e8e9df7cc92b89e7d09d9"}"#,
            "\n"
        );
        fs::write(root.path().join("events.jsonl"), line).unwrap();

        assert!(matches!(
            EventLog::open(root.path()),
            Err(StoreError::InvalidChecksum { line: 1 })
        ));
    }

    #[test]
    fn tampering_is_detected_without_repairing_the_arche() {
        let root = tempdir().unwrap();
        let mut log = EventLog::open(root.path()).unwrap();
        log.append(
            DurableEvent::NativeObjectChanged {
                head: head("first", 10),
                outbound: Vec::new(),
            },
            10,
        )
        .unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(root.path().join("events.jsonl"))
            .unwrap();
        writeln!(file, "{{\"partial\":true}}").unwrap();
        file.sync_all().unwrap();

        assert!(matches!(
            EventLog::open(root.path()),
            Err(StoreError::InvalidJson { .. })
        ));
    }

    #[test]
    fn durable_store_rejects_duplicate_link_without_writing_it() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        for name in ["first", "second"] {
            let result = store.append(
                DurableEvent::PersonaCreated(Persona {
                    id: PersonaId::new(),
                    public_key: NostrPublicKey::parse(format!("npub-{name}")).unwrap(),
                    display_name: name.to_owned(),
                    reddit_account: Some(RedditAccountId::parse("shared").unwrap()),
                }),
                10,
            );
            if name == "first" {
                result.unwrap();
            } else {
                assert!(matches!(result, Err(StoreError::Domain(_))));
            }
        }
        assert_eq!(
            EventLog::open(root.path())
                .unwrap()
                .read_all()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn operation_lifecycle_is_validated_before_append() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let operation_id = OperationId::new();
        for state in [
            OperationState::Queued,
            OperationState::Running,
            OperationState::Succeeded,
        ] {
            store
                .append(
                    DurableEvent::OperationChanged {
                        operation_id,
                        state,
                    },
                    10,
                )
                .unwrap();
        }
        assert!(matches!(
            store.append(
                DurableEvent::OperationChanged {
                    operation_id,
                    state: OperationState::Queued,
                },
                11,
            ),
            Err(StoreError::Domain(_))
        ));
        assert_eq!(
            EventLog::open(root.path())
                .unwrap()
                .read_all()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn concurrent_writers_preserve_one_valid_checksum_chain() {
        let root = tempdir().unwrap();
        let barrier = Arc::new(Barrier::new(9));
        let handles = (0..8)
            .map(|index| {
                let path = root.path().to_owned();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let mut log = EventLog::open(path).unwrap();
                    barrier.wait();
                    log.append(
                        DurableEvent::PersonaCreated(Persona {
                            id: PersonaId::new(),
                            public_key: NostrPublicKey::parse(format!("npub-{index}")).unwrap(),
                            display_name: format!("Persona {index}"),
                            reddit_account: None,
                        }),
                        index,
                    )
                    .unwrap();
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }

        let log = EventLog::open(root.path()).unwrap();
        assert_eq!(log.read_all().unwrap().len(), 8);
        assert_eq!(log.replay().unwrap().personas.iter().count(), 8);
    }

    #[test]
    fn stale_store_reloads_under_lock_before_validating_and_appending() {
        let root = tempdir().unwrap();
        let mut first = DurableStore::open(root.path()).unwrap();
        let mut second = DurableStore::open(root.path()).unwrap();
        for (store, name, recorded_at) in [
            (&mut first, "first", 10_u64),
            (&mut second, "second", 11_u64),
        ] {
            store
                .append(
                    DurableEvent::PersonaCreated(Persona {
                        id: PersonaId::new(),
                        public_key: NostrPublicKey::parse(format!("npub-{name}")).unwrap(),
                        display_name: name.to_owned(),
                        reddit_account: None,
                    }),
                    recorded_at,
                )
                .unwrap();
        }

        assert_eq!(second.state().personas.iter().count(), 2);
        assert_eq!(
            EventLog::open(root.path())
                .unwrap()
                .replay()
                .unwrap()
                .personas
                .iter()
                .count(),
            2
        );
    }

    #[test]
    fn local_credential_vault_encrypts_round_trips_and_deletes() {
        let root = tempdir().unwrap();
        let vault = LocalCredentialVault {
            root: root.path().to_owned(),
            namespace: "nostr".to_owned(),
        };

        vault.set("persona-a", "nsec-private-material").unwrap();
        assert_eq!(vault.get("persona-a").unwrap(), "nsec-private-material");
        let stored = fs::read_to_string(vault.path("persona-a")).unwrap();
        assert!(!stored.contains("nsec-private-material"));
        vault.delete("persona-a").unwrap();
        assert!(matches!(vault.get("persona-a"), Err(StoreError::NotFound)));
    }

    #[test]
    fn platform_keyring_timeout_returns_control_to_the_local_fallback() {
        let started = Instant::now();
        let (result, timed_out) = try_platform_keyring_with_timeout(
            || {
                thread::sleep(Duration::from_millis(250));
                Some("late")
            },
            Duration::from_millis(10),
        );

        assert_eq!(result, None);
        assert!(timed_out);
        assert!(started.elapsed() < Duration::from_millis(150));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_platform_keyring_runs_on_the_calling_thread() {
        let caller = thread::current().id();
        let observed = try_platform_keyring(|| Some(thread::current().id()));

        assert_eq!(observed, Some(caller));
    }

    #[test]
    fn local_credential_vault_binds_ciphertext_to_identity() {
        let root = tempdir().unwrap();
        let vault = LocalCredentialVault {
            root: root.path().to_owned(),
            namespace: "nostr".to_owned(),
        };
        vault.set("persona-a", "secret-a").unwrap();
        vault.set("persona-b", "secret-b").unwrap();
        fs::copy(vault.path("persona-a"), vault.path("persona-b")).unwrap();

        assert!(matches!(
            vault.get("persona-b"),
            Err(StoreError::Encryption(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn local_credential_vault_uses_private_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempdir().unwrap();
        let vault = LocalCredentialVault {
            root: root.path().to_owned(),
            namespace: "nostr".to_owned(),
        };
        vault.set("persona-a", "secret-a").unwrap();

        assert_eq!(
            fs::metadata(root.path().join(".credential-key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(vault.path("persona-a"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn oversized_event_record_is_rejected_before_json_allocation_grows_unbounded() {
        let root = tempdir().unwrap();
        let log = EventLog::open(root.path()).unwrap();
        fs::write(
            root.path().join("events.jsonl"),
            vec![b'x'; 4 * 1024 * 1024 + 1],
        )
        .unwrap();
        assert!(matches!(
            log.read_all(),
            Err(StoreError::RecordTooLarge {
                line: 1,
                max: 4_194_304
            })
        ));
    }

    #[test]
    fn malformed_deserialized_domain_event_is_rejected_without_append() {
        let root = tempdir().unwrap();
        let mut log = EventLog::open(root.path()).unwrap();
        let event = DurableEvent::PersonaCreated(hydra_domain::Persona {
            id: PersonaId::new(),
            public_key: NostrPublicKey::parse("public").unwrap(),
            display_name: "x".repeat(hydra_domain::Persona::MAX_DISPLAY_NAME_LEN + 1),
            reddit_account: None,
        });
        assert!(matches!(log.append(event, 1), Err(StoreError::Domain(_))));
        assert!(log.read_all().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn state_root_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let real = directory.path().join("real");
        fs::create_dir(&real).unwrap();
        let linked = directory.path().join("linked");
        symlink(&real, &linked).unwrap();
        assert!(EventLog::open(linked).is_err());
    }
}
