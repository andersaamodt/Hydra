use std::{
    fs::{self, File},
    io::Write as _,
    iter,
    path::Path,
};

use age::{Decryptor, Encryptor, secrecy::SecretString};
use hydra_domain::{DurableEvent, NostrPublicKey, PersonaId};
use hydra_media::MediaStore;
use hydra_store::{DurableStore, EventLog, SettingsStore};
use nostr::ToBech32;
use serde::{Deserialize, Serialize};

use crate::{AppError, PersonaCredential, SecretStore};

const MANIFEST_PATH: &str = "hydra-backup.json";
const SCHEMA: &str = "hydra-encrypted-backup/v1";

#[derive(Debug, Serialize, Deserialize)]
struct BackupManifest {
    schema: String,
    persona_secrets: Vec<PersonaSecret>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersonaSecret {
    persona_id: PersonaId,
    secret: String,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct BackupService;

impl BackupService {
    /// Writes a portable, passphrase-encrypted age archive without exposing
    /// persona keys or private local records outside the encrypted stream.
    ///
    /// # Errors
    ///
    /// Returns an error for weak input, credential access, or failed I/O.
    pub fn export(
        root: impl AsRef<Path>,
        secrets: &impl SecretStore,
        persona_id: PersonaId,
        passphrase: &str,
        destination: impl AsRef<Path>,
    ) -> Result<(), AppError> {
        validate_passphrase(passphrase)?;
        let root = root.as_ref();
        let store = DurableStore::open(root)?;
        let persona = store
            .state()
            .personas
            .get(persona_id)
            .ok_or(hydra_domain::DomainError::MissingPersona)?;
        let manifest = BackupManifest {
            schema: SCHEMA.to_owned(),
            persona_secrets: vec![PersonaSecret {
                persona_id,
                secret: secrets.get(persona_id)?,
            }],
        };
        let staging = tempfile::tempdir()?;
        write_persona_archive(root, staging.path(), persona_id, &persona.public_key)?;
        write_portable_event_log(staging.path())?;
        let destination = destination.as_ref();
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temporary = tempfile::NamedTempFile::new_in(parent)?;
        let output = temporary.reopen()?;
        let encryptor = Encryptor::with_user_passphrase(SecretString::from(passphrase.to_owned()));
        let mut encrypted = encryptor
            .wrap_output(output)
            .map_err(|error| AppError::Backup(error.to_string()))?;
        {
            let mut archive = tar::Builder::new(&mut encrypted);
            append_bytes(&mut archive, MANIFEST_PATH, &serde_json::to_vec(&manifest)?)?;
            append_if_file(&mut archive, staging.path(), "events.jsonl")?;
            append_if_file(&mut archive, staging.path(), "settings.yaml")?;
            append_media(&mut archive, root, staging.path())?;
            archive.finish()?;
        }
        let output = encrypted
            .finish()
            .map_err(|error| AppError::Backup(error.to_string()))?;
        output.sync_all()?;
        verify_backup_file(temporary.path(), passphrase)?;
        temporary
            .persist(destination)
            .map_err(|error| AppError::Io(error.error))?;
        sync_parent(destination)?;
        Ok(())
    }

    /// Restores an age archive into a new, empty Hydra root and credential
    /// vault. Existing Hydra data is never merged or overwritten.
    ///
    /// # Errors
    ///
    /// Returns an error for a wrong passphrase, unsafe/invalid archive, a
    /// non-empty destination, key mismatch, credential failure, or failed I/O.
    pub fn restore(
        archive_path: impl AsRef<Path>,
        root: impl AsRef<Path>,
        secrets: &impl SecretStore,
        passphrase: String,
    ) -> Result<(), AppError> {
        validate_passphrase(&passphrase)?;
        let root = root.as_ref();
        ensure_empty_destination(root)?;
        let parent = root
            .parent()
            .ok_or_else(|| AppError::Backup("restore destination has no parent".to_owned()))?;
        fs::create_dir_all(parent)?;
        let staging = tempfile::Builder::new()
            .prefix(".hydra-restore-")
            .tempdir_in(parent)?;
        let input = File::open(archive_path)?;
        let decryptor =
            Decryptor::new(input).map_err(|error| AppError::Backup(error.to_string()))?;
        let identity = age::scrypt::Identity::new(SecretString::from(passphrase));
        let reader = decryptor
            .decrypt(iter::once(&identity as &dyn age::Identity))
            .map_err(|error| AppError::Backup(error.to_string()))?;
        let mut archive = tar::Archive::new(reader);
        unpack_backup(&mut archive, staging.path())?;

        let manifest: BackupManifest =
            serde_json::from_slice(&fs::read(staging.path().join(MANIFEST_PATH))?)?;
        if manifest.schema != SCHEMA {
            return Err(AppError::Backup("unsupported backup schema".to_owned()));
        }
        let restored = DurableStore::open(staging.path())?;
        validate_persona_secrets(&restored, &manifest)?;
        let media = MediaStore::new(staging.path());
        for item in restored.state().media.values() {
            media.verify(item)?;
        }
        drop(restored);
        fs::remove_file(staging.path().join(MANIFEST_PATH))?;

        if root.exists() {
            fs::remove_dir(root)?;
        }
        let staging = staging.keep();
        fs::rename(&staging, root)?;
        let mut installed = Vec::new();
        for persona in manifest.persona_secrets {
            if let Err(error) = secrets.set(persona.persona_id, &persona.secret) {
                for persona_id in installed {
                    let _ = secrets.delete(persona_id);
                }
                let _ = fs::remove_dir_all(root);
                return Err(error);
            }
            installed.push(persona.persona_id);
        }
        sync_parent(root)?;
        Ok(())
    }
}

fn write_portable_event_log(root: &Path) -> Result<(), AppError> {
    let events = EventLog::open(root)?.read_all()?;
    let path = root.join("events.jsonl");
    let mut file = File::create(path)?;
    for envelope in events {
        serde_json::to_writer(&mut file, &envelope)?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    Ok(())
}

fn write_persona_archive(
    source: &Path,
    destination: &Path,
    persona_id: PersonaId,
    public_key: &NostrPublicKey,
) -> Result<(), AppError> {
    let source_log = EventLog::open(source)?;
    let mut destination_log = EventLog::open(destination)?;
    let envelopes = source_log.read_all()?;
    let included_event_ids = envelopes
        .iter()
        .filter(|envelope| event_belongs_to_persona(&envelope.event, persona_id, public_key))
        .flat_map(|envelope| outbound_ids(&envelope.event))
        .collect::<std::collections::BTreeSet<_>>();

    for envelope in envelopes {
        let include = event_belongs_to_persona(&envelope.event, persona_id, public_key)
            || matches!(
                &envelope.event,
                DurableEvent::DeliveryRecorded { event_id, .. } if included_event_ids.contains(event_id)
            );
        if include {
            destination_log.append(envelope.event, envelope.recorded_at)?;
        }
    }

    let mut settings = SettingsStore::new(source).load()?;
    settings.active_persona_id = Some(persona_id.to_string());
    settings
        .persona_crosspost_defaults
        .retain(|persona, _| persona == &persona_id.to_string());
    SettingsStore::new(destination).save(&settings)?;
    Ok(())
}

fn event_belongs_to_persona(
    event: &DurableEvent,
    persona_id: PersonaId,
    public_key: &NostrPublicKey,
) -> bool {
    match event {
        DurableEvent::PersonaCreated(persona) | DurableEvent::PersonaUpdated(persona) => {
            persona.id == persona_id
        }
        DurableEvent::PersonaProfilePublished { persona, .. }
        | DurableEvent::InboxRelaysChanged { persona, .. }
        | DurableEvent::PersonaRelaysChanged { persona, .. }
        | DurableEvent::MediaPublished { persona, .. }
        | DurableEvent::MediaPreservedFor { persona, .. }
        | DurableEvent::ObjectDisowningRequested { persona, .. }
        | DurableEvent::PublicEventQueued { persona, .. } => *persona == persona_id,
        DurableEvent::RedditIdentityProofPublished { proof, .. } => proof.persona == persona_id,
        DurableEvent::NativeObjectChanged { head, .. } => &head.author == public_key,
        DurableEvent::RemoteEventReceived { .. } => true,
        DurableEvent::ArchiveCaptured(manifest) => manifest.observer == persona_id,
        DurableEvent::ProjectionChanged { projection, .. } => projection.persona == persona_id,
        DurableEvent::ReactionRecorded { reaction, .. } => &reaction.actor == public_key,
        DurableEvent::PrivateRecordStored { record, .. } => record.persona == persona_id,
        DurableEvent::FollowChanged { follow, .. } => follow.persona == persona_id,
        DurableEvent::PublicFollowSetPublished { set, .. } => set.persona == persona_id,
        DurableEvent::CommunitySubscriptionChanged { subscription, .. } => {
            subscription.persona == persona_id
        }
        DurableEvent::BlockChanged { block, .. } => block.persona == persona_id,
        DurableEvent::MediaPreserved(_)
        | DurableEvent::DeliveryRecorded { .. }
        | DurableEvent::OperationChanged { .. }
        | DurableEvent::ContinuityWorkflowChanged(_) => false,
    }
}

fn outbound_ids(event: &DurableEvent) -> Vec<String> {
    match event {
        DurableEvent::PersonaProfilePublished { outbound, .. }
        | DurableEvent::RedditIdentityProofPublished { outbound, .. }
        | DurableEvent::InboxRelaysChanged { outbound, .. }
        | DurableEvent::PersonaRelaysChanged { outbound, .. }
        | DurableEvent::ReactionRecorded { outbound, .. }
        | DurableEvent::MediaPublished { outbound, .. }
        | DurableEvent::PublicFollowSetPublished { outbound, .. }
        | DurableEvent::ObjectDisowningRequested { outbound, .. } => {
            vec![outbound.event_id.clone()]
        }
        DurableEvent::PublicEventQueued { outbound, .. } => vec![outbound.event_id.clone()],
        DurableEvent::NativeObjectChanged { outbound, .. }
        | DurableEvent::PrivateRecordStored { outbound, .. }
        | DurableEvent::FollowChanged { outbound, .. }
        | DurableEvent::CommunitySubscriptionChanged { outbound, .. }
        | DurableEvent::BlockChanged { outbound, .. } => {
            outbound.iter().map(|item| item.event_id.clone()).collect()
        }
        DurableEvent::ProjectionChanged { outbound, .. } => {
            outbound.iter().map(|item| item.event_id.clone()).collect()
        }
        DurableEvent::PersonaCreated(_)
        | DurableEvent::PersonaUpdated(_)
        | DurableEvent::RemoteEventReceived { .. }
        | DurableEvent::MediaPreserved(_)
        | DurableEvent::MediaPreservedFor { .. }
        | DurableEvent::ArchiveCaptured(_)
        | DurableEvent::DeliveryRecorded { .. }
        | DurableEvent::OperationChanged { .. }
        | DurableEvent::ContinuityWorkflowChanged(_) => Vec::new(),
    }
}

fn verify_backup_file(path: &Path, passphrase: &str) -> Result<(), AppError> {
    let input = File::open(path)?;
    let decryptor = Decryptor::new(input).map_err(|error| AppError::Backup(error.to_string()))?;
    let identity = age::scrypt::Identity::new(SecretString::from(passphrase.to_owned()));
    let reader = decryptor
        .decrypt(iter::once(&identity as &dyn age::Identity))
        .map_err(|error| AppError::Backup(error.to_string()))?;
    let staging = tempfile::tempdir()?;
    unpack_backup(&mut tar::Archive::new(reader), staging.path())?;
    let manifest: BackupManifest =
        serde_json::from_slice(&fs::read(staging.path().join(MANIFEST_PATH))?)?;
    if manifest.schema != SCHEMA {
        return Err(AppError::Backup("unsupported backup schema".to_owned()));
    }
    let store = DurableStore::open(staging.path())?;
    validate_persona_secrets(&store, &manifest)?;
    let media = MediaStore::new(staging.path());
    for item in store.state().media.values() {
        media.verify(item)?;
    }
    Ok(())
}

fn validate_passphrase(passphrase: &str) -> Result<(), AppError> {
    if passphrase.chars().count() < 12 {
        return Err(AppError::Backup(
            "backup passphrase must contain at least 12 characters".to_owned(),
        ));
    }
    Ok(())
}

fn validate_persona_secrets(
    store: &DurableStore,
    manifest: &BackupManifest,
) -> Result<(), AppError> {
    if store.state().personas.iter().count() != manifest.persona_secrets.len() {
        return Err(AppError::Backup(
            "backup persona records and credentials do not match".to_owned(),
        ));
    }
    for item in &manifest.persona_secrets {
        let persona = store
            .state()
            .personas
            .get(item.persona_id)
            .ok_or_else(|| AppError::Backup("backup persona is missing".to_owned()))?;
        let public = PersonaCredential::decode(&item.secret)?
            .public_key()?
            .to_bech32()
            .map_err(|error| AppError::Backup(error.to_string()))?;
        if public != persona.public_key.as_str() {
            return Err(AppError::PersonaKeyMismatch);
        }
    }
    Ok(())
}

fn append_bytes(
    archive: &mut tar::Builder<&mut age::stream::StreamWriter<File>>,
    path: &str,
    bytes: &[u8],
) -> Result<(), AppError> {
    let mut header = tar::Header::new_gnu();
    header
        .set_size(u64::try_from(bytes.len()).map_err(|error| AppError::Backup(error.to_string()))?);
    header.set_mode(0o600);
    header.set_cksum();
    archive.append_data(&mut header, path, bytes)?;
    Ok(())
}

fn append_if_file(
    archive: &mut tar::Builder<&mut age::stream::StreamWriter<File>>,
    root: &Path,
    relative: &str,
) -> Result<(), AppError> {
    let path = root.join(relative);
    if path.is_file() {
        archive.append_path_with_name(path, relative)?;
    }
    Ok(())
}

fn append_media(
    archive: &mut tar::Builder<&mut age::stream::StreamWriter<File>>,
    source_root: &Path,
    selected_root: &Path,
) -> Result<(), AppError> {
    let media = source_root.join("media");
    if !media.is_dir() {
        return Ok(());
    }
    let selected = DurableStore::open(selected_root)?
        .state()
        .media
        .values()
        .map(|manifest| manifest.sha256.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for entry in fs::read_dir(media)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| AppError::Backup("media filename is not UTF-8".to_owned()))?;
        if entry.file_type()?.is_file()
            && name.len() == 64
            && name.bytes().all(|byte| byte.is_ascii_hexdigit())
            && selected.contains(name)
        {
            archive.append_path_with_name(entry.path(), Path::new("media").join(name))?;
        }
    }
    Ok(())
}

fn ensure_empty_destination(root: &Path) -> Result<(), AppError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(AppError::Backup(
                "restore destination must be a real directory".to_owned(),
            ));
        }
        Ok(_) if fs::read_dir(root)?.next().transpose()?.is_some() => {
            return Err(AppError::Backup(
                "restore destination must be empty".to_owned(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn unpack_backup<R: std::io::Read>(
    archive: &mut tar::Archive<R>,
    destination: &Path,
) -> Result<(), AppError> {
    const MAX_ENTRIES: usize = 100_000;
    const MAX_EXTRACTED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
    let mut entries = 0_usize;
    let mut extracted = 0_u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        entries = entries.saturating_add(1);
        extracted = extracted
            .checked_add(entry.size())
            .ok_or_else(|| AppError::Backup("backup size overflow".to_owned()))?;
        if entries > MAX_ENTRIES || extracted > MAX_EXTRACTED_BYTES {
            return Err(AppError::Backup(
                "backup exceeds Hydra's extraction safety limits".to_owned(),
            ));
        }
        if !entry.header().entry_type().is_file() {
            return Err(AppError::Backup(
                "backup contains a non-file entry".to_owned(),
            ));
        }
        let path = entry.path()?.into_owned();
        if !allowed_archive_path(&path) {
            return Err(AppError::Backup(format!(
                "backup contains an unexpected path: {}",
                path.display()
            )));
        }
        if entry.size() > max_archive_entry_bytes(&path) {
            return Err(AppError::Backup(format!(
                "backup entry exceeds its safety limit: {}",
                path.display()
            )));
        }
        if !entry.unpack_in(destination)? {
            return Err(AppError::Backup(
                "backup path escaped destination".to_owned(),
            ));
        }
    }
    Ok(())
}

fn max_archive_entry_bytes(path: &Path) -> u64 {
    if path == Path::new(MANIFEST_PATH) || path == Path::new("settings.yaml") {
        1024 * 1024
    } else if path == Path::new("events.jsonl") {
        1024 * 1024 * 1024
    } else {
        hydra_domain::MediaManifest::MAX_BYTES
    }
}

fn allowed_archive_path(path: &Path) -> bool {
    if path == Path::new(MANIFEST_PATH)
        || path == Path::new("events.jsonl")
        || path == Path::new("settings.yaml")
    {
        return true;
    }
    let parts = path.components().collect::<Vec<_>>();
    parts.len() == 2
        && parts[0].as_os_str() == "media"
        && parts[1].as_os_str().to_str().is_some_and(|name| {
            name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn sync_parent(path: &Path) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

    use hydra_store::{Settings, SettingsStore};
    use tempfile::tempdir;

    use super::*;
    use crate::PersonaService;

    #[derive(Debug, Default, Clone)]
    struct TestSecrets(Rc<RefCell<BTreeMap<PersonaId, String>>>);

    impl SecretStore for TestSecrets {
        fn set(&self, persona: PersonaId, secret: &str) -> Result<(), AppError> {
            self.0.borrow_mut().insert(persona, secret.to_owned());
            Ok(())
        }

        fn get(&self, persona: PersonaId) -> Result<String, AppError> {
            self.0
                .borrow()
                .get(&persona)
                .cloned()
                .ok_or_else(|| AppError::Credential("missing test key".to_owned()))
        }

        fn delete(&self, persona: PersonaId) -> Result<(), AppError> {
            self.0.borrow_mut().remove(&persona);
            Ok(())
        }
    }

    #[test]
    fn encrypted_backup_round_trips_personas_settings_and_event_history() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let restored = directory.path().join("restored");
        let archive = directory.path().join("hydra-backup.age");
        let secrets = TestSecrets::default();
        let mut store = DurableStore::open(&source).unwrap();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Backup Alice".to_owned(), 10)
            .unwrap();
        let other = PersonaService::new(secrets.clone())
            .create(&mut store, "Separate Bob".to_owned(), 11)
            .unwrap();
        SettingsStore::new(&source)
            .save(&Settings {
                relays: vec!["wss://relay.example".to_owned()],
                inbox_relays: vec!["wss://inbox.example".to_owned()],
                replication_threshold: 1,
                ..Settings::default()
            })
            .unwrap();
        let secret = secrets.get(persona.id).unwrap();

        BackupService::export(
            &source,
            &secrets,
            persona.id,
            "correct horse battery staple",
            &archive,
        )
        .unwrap();
        let encrypted = fs::read(&archive).unwrap();
        assert!(
            !encrypted
                .windows(secret.len())
                .any(|part| part == secret.as_bytes())
        );
        assert!(!encrypted.windows(12).any(|part| part == b"Backup Alice"));

        let restored_secrets = TestSecrets::default();
        BackupService::restore(
            &archive,
            &restored,
            &restored_secrets,
            "correct horse battery staple".to_owned(),
        )
        .unwrap();
        assert_eq!(
            DurableStore::open(&restored)
                .unwrap()
                .state()
                .personas
                .iter()
                .count(),
            1
        );
        assert_eq!(restored_secrets.get(persona.id).unwrap(), secret);
        assert!(restored_secrets.get(other.id).is_err());
        assert_eq!(
            SettingsStore::new(&restored).load().unwrap().relays,
            vec!["wss://relay.example"]
        );
        assert!(!restored.join(MANIFEST_PATH).exists());
    }

    #[test]
    fn restore_rejects_wrong_passphrase_and_existing_data() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let archive = directory.path().join("hydra-backup.age");
        let secrets = TestSecrets::default();
        let mut store = DurableStore::open(&source).unwrap();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        BackupService::export(
            &source,
            &secrets,
            persona.id,
            "correct horse battery staple",
            &archive,
        )
        .unwrap();

        let wrong_target = directory.path().join("wrong");
        assert!(
            BackupService::restore(
                &archive,
                &wrong_target,
                &TestSecrets::default(),
                "this passphrase is wrong".to_owned(),
            )
            .is_err()
        );
        assert!(!wrong_target.exists());

        let occupied = directory.path().join("occupied");
        fs::create_dir(&occupied).unwrap();
        fs::write(occupied.join("keep"), b"mine").unwrap();
        assert!(
            BackupService::restore(
                &archive,
                &occupied,
                &TestSecrets::default(),
                "correct horse battery staple".to_owned(),
            )
            .is_err()
        );
        assert_eq!(fs::read(occupied.join("keep")).unwrap(), b"mine");
    }

    #[test]
    fn archive_path_allowlist_rejects_traversal_and_symlink_targets() {
        assert!(allowed_archive_path(Path::new("events.jsonl")));
        assert!(allowed_archive_path(Path::new(
            "media/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        )));
        assert!(!allowed_archive_path(Path::new("../events.jsonl")));
        assert!(!allowed_archive_path(Path::new("media/not-a-hash")));
        assert!(!allowed_archive_path(Path::new("secrets.txt")));
        assert_eq!(
            max_archive_entry_bytes(Path::new(MANIFEST_PATH)),
            1024 * 1024
        );
        assert_eq!(
            max_archive_entry_bytes(Path::new(
                "media/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            )),
            hydra_domain::MediaManifest::MAX_BYTES
        );
    }

    #[cfg(unix)]
    #[test]
    fn backup_and_restore_never_follow_destination_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let secrets = TestSecrets::default();
        let mut store = DurableStore::open(&source).unwrap();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        let outside_file = directory.path().join("outside.age");
        fs::write(&outside_file, b"must remain unchanged").unwrap();
        let archive = directory.path().join("backup.age");
        symlink(&outside_file, &archive).unwrap();

        BackupService::export(
            &source,
            &secrets,
            persona.id,
            "correct horse battery staple",
            &archive,
        )
        .unwrap();
        assert_eq!(fs::read(&outside_file).unwrap(), b"must remain unchanged");
        assert!(
            !fs::symlink_metadata(&archive)
                .unwrap()
                .file_type()
                .is_symlink()
        );

        let outside_directory = directory.path().join("outside-restore");
        fs::create_dir(&outside_directory).unwrap();
        let restore = directory.path().join("restore");
        symlink(&outside_directory, &restore).unwrap();
        assert!(
            BackupService::restore(
                &archive,
                &restore,
                &TestSecrets::default(),
                "correct horse battery staple".to_owned(),
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(outside_directory).unwrap().count(), 0);
    }
}
