use std::{
    collections::BTreeMap,
    env,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

use hydra_app::{AppError, ArchiveService, PersonaService, ProjectionService, SecretStore};
use hydra_domain::{
    AnchorId, ArchiveId, ArchiveManifest, BigStickState, ContinuityState, ContinuityWorkflow,
    DeliveryState, DurableEvent, ExternalId, ObjectHead, ObjectKind, OperationId, PersonaId,
    PreservationLevel, Projection, ProjectionId, ProjectionState, ReddactedState, RedditAccountId,
};
use hydra_store::{DurableStore, HeadStore, LocalCredentialVault, try_platform_keyring};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    Attribution, RedditAdapter, RedditError, RedditFullname, RedditIdentity, RedditThing,
    SubmitComment, SubmitPost, render_markdown,
};

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error(transparent)]
    Reddit(#[from] RedditError),
    #[error(transparent)]
    App(#[from] AppError),
    #[error("Reddit credential vault failed: {0}")]
    Credential(String),
    #[error("the selected object cannot be projected to this destination")]
    InvalidDestination,
    #[error("the projection payload no longer matches its durable journal")]
    PayloadChanged,
    #[error("the projection is not queued for execution")]
    NotQueued,
    #[error("the Hydra object has not reached the required relay replication threshold")]
    NotReplicated,
    #[error("withdrawn content requires an explicit restore action")]
    Withdrawn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedditCredential {
    pub identity: RedditIdentity,
    #[serde(default)]
    pub client_id: Option<String>,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: u64,
    pub scope: String,
}

pub trait RedditCredentialStore {
    /// Stores one persona-scoped Reddit credential.
    ///
    /// # Errors
    ///
    /// Returns an error when secure persistence fails.
    fn set(&self, persona: PersonaId, credential: &RedditCredential) -> Result<(), BridgeError>;

    /// Loads one persona-scoped Reddit credential.
    ///
    /// # Errors
    ///
    /// Returns an error when the credential is absent or cannot be decoded.
    fn get(&self, persona: PersonaId) -> Result<RedditCredential, BridgeError>;

    /// Deletes one persona-scoped Reddit credential.
    ///
    /// # Errors
    ///
    /// Returns an error when secure deletion fails.
    fn delete(&self, persona: PersonaId) -> Result<(), BridgeError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformRedditCredentialStore;

impl PlatformRedditCredentialStore {
    const SERVICE: &'static str = "org.hydra.desktop.reddit";

    fn entry(persona: PersonaId) -> Result<Entry, BridgeError> {
        Entry::new(Self::SERVICE, &persona.to_string())
            .map_err(|error| BridgeError::Credential(error.to_string()))
    }

    fn local() -> Result<LocalCredentialVault, BridgeError> {
        LocalCredentialVault::for_hydra("reddit")
            .map_err(|error| BridgeError::Credential(error.to_string()))
    }

    fn cache() -> &'static Mutex<BTreeMap<String, RedditCredential>> {
        static CACHE: OnceLock<Mutex<BTreeMap<String, RedditCredential>>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
    }

    fn cache_key(persona: PersonaId) -> Option<String> {
        let root = env::var_os("HYDRA_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join("hydra")))?;
        Some(format!("{}\0{persona}", root.display()))
    }

    #[must_use]
    pub fn custody_label(persona: PersonaId) -> &'static str {
        if Self::local().is_ok_and(|vault| vault.get(&persona.to_string()).is_ok()) {
            return "encrypted local fallback";
        }
        if Self::cache_key(persona).is_some_and(|key| {
            Self::cache()
                .lock()
                .is_ok_and(|cache| cache.contains_key(&key))
        }) {
            return "secure credential vault";
        }
        if try_platform_keyring(move || Self::entry(persona).ok()?.get_password().ok()).is_some() {
            return "system credential vault";
        }
        "credential unavailable"
    }
}

impl RedditCredentialStore for PlatformRedditCredentialStore {
    fn set(&self, persona: PersonaId, credential: &RedditCredential) -> Result<(), BridgeError> {
        let encoded = serde_json::to_string(credential)
            .map_err(|error| BridgeError::Credential(error.to_string()))?;
        let keyring_value = encoded.clone();
        let keyring_stored = try_platform_keyring(move || {
            let entry = Self::entry(persona).ok()?;
            entry.set_password(&keyring_value).ok()?;
            entry.get_password().ok()
        })
        .is_some_and(|stored| stored == encoded);
        let local = Self::local()?;
        let result = if keyring_stored {
            let _ = local.delete(&persona.to_string());
            Ok(())
        } else {
            local
                .set(&persona.to_string(), &encoded)
                .map_err(|error| BridgeError::Credential(error.to_string()))
        };
        result?;
        if let Some(key) = Self::cache_key(persona) {
            Self::cache()
                .lock()
                .map_err(|error| BridgeError::Credential(error.to_string()))?
                .insert(key, credential.clone());
        }
        Ok(())
    }

    fn get(&self, persona: PersonaId) -> Result<RedditCredential, BridgeError> {
        let cache_key = Self::cache_key(persona);
        if let Some(credential) = cache_key
            .as_ref()
            .and_then(|key| Self::cache().lock().ok()?.get(key).cloned())
        {
            return Ok(credential);
        }
        let encoded = if let Ok(encoded) = Self::local()?.get(&persona.to_string()) {
            encoded
        } else {
            try_platform_keyring(move || Self::entry(persona).ok()?.get_password().ok())
                .ok_or_else(|| BridgeError::Credential("credential not found".to_owned()))?
        };
        let credential: RedditCredential = serde_json::from_str(&encoded)
            .map_err(|error| BridgeError::Credential(error.to_string()))?;
        if let Some(key) = cache_key {
            Self::cache()
                .lock()
                .map_err(|error| BridgeError::Credential(error.to_string()))?
                .insert(key, credential.clone());
        }
        Ok(credential)
    }

    fn delete(&self, persona: PersonaId) -> Result<(), BridgeError> {
        let local_deleted = Self::local()?.delete(&persona.to_string()).is_ok();
        let keyring_deleted =
            try_platform_keyring(move || Self::entry(persona).ok()?.delete_credential().ok())
                .is_some();
        if keyring_deleted || local_deleted {
            if let Some(key) = Self::cache_key(persona) {
                Self::cache()
                    .lock()
                    .map_err(|error| BridgeError::Credential(error.to_string()))?
                    .remove(&key);
            }
            Ok(())
        } else {
            Err(BridgeError::Credential("credential not found".to_owned()))
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct MemoryRedditCredentialStore(Arc<Mutex<BTreeMap<PersonaId, RedditCredential>>>);

impl RedditCredentialStore for MemoryRedditCredentialStore {
    fn set(&self, persona: PersonaId, credential: &RedditCredential) -> Result<(), BridgeError> {
        self.0
            .lock()
            .map_err(|error| lock_error(&error))?
            .insert(persona, credential.clone());
        Ok(())
    }

    fn get(&self, persona: PersonaId) -> Result<RedditCredential, BridgeError> {
        self.0
            .lock()
            .map_err(|error| lock_error(&error))?
            .get(&persona)
            .cloned()
            .ok_or_else(|| BridgeError::Credential("credential not found".to_owned()))
    }

    fn delete(&self, persona: PersonaId) -> Result<(), BridgeError> {
        self.0
            .lock()
            .map_err(|error| lock_error(&error))?
            .remove(&persona);
        Ok(())
    }
}

pub struct RedditLinkService<C> {
    credentials: C,
}

impl<C: RedditCredentialStore> RedditLinkService<C> {
    #[must_use]
    pub fn new(credentials: C) -> Self {
        Self { credentials }
    }

    /// Completes a verified one-to-one account link, rolling the credential
    /// back if the durable identity update fails.
    ///
    /// # Errors
    ///
    /// Returns an error for failed identity lookup, secure storage, conflicting
    /// account linkage, or durable append failure.
    pub fn link<S: SecretStore, A: RedditAdapter>(
        &self,
        personas: &PersonaService<S>,
        store: &mut DurableStore,
        persona: PersonaId,
        adapter: &A,
        credential: &RedditCredential,
        recorded_at: u64,
    ) -> Result<RedditIdentity, BridgeError> {
        let identity = adapter.identity()?;
        if identity != credential.identity {
            return Err(BridgeError::Credential(
                "OAuth credential identity does not match Reddit".to_owned(),
            ));
        }
        self.credentials.set(persona, credential)?;
        let account =
            RedditAccountId::parse(identity.account_id.clone()).map_err(AppError::Domain)?;
        if let Err(error) = personas.set_reddit_account(store, persona, Some(account), recorded_at)
        {
            self.credentials.delete(persona)?;
            return Err(error.into());
        }
        Ok(identity)
    }

    /// Removes the local credential and durable public account link.
    ///
    /// # Errors
    ///
    /// Returns an error when secure deletion or durable unlinking fails.
    pub fn unlink<S: SecretStore>(
        &self,
        personas: &PersonaService<S>,
        store: &mut DurableStore,
        persona: PersonaId,
        recorded_at: u64,
    ) -> Result<(), BridgeError> {
        self.credentials.delete(persona)?;
        personas.set_reddit_account(store, persona, None, recorded_at)?;
        Ok(())
    }
}

pub struct BridgeService<S, A> {
    projections: ProjectionService<S>,
    adapter: A,
}

pub struct QueuePost<'a> {
    pub persona: PersonaId,
    pub anchor: &'a AnchorId,
    pub subreddit: &'a str,
    pub attribution: Attribution<'a>,
    pub relays: Vec<String>,
    pub recorded_at: u64,
}

pub struct QueueComment<'a> {
    pub persona: PersonaId,
    pub anchor: &'a AnchorId,
    pub parent: &'a RedditFullname,
    pub attribution: Attribution<'a>,
    pub relays: Vec<String>,
    pub recorded_at: u64,
}

pub struct ProjectionAction {
    pub projection: ProjectionId,
    pub relays: Vec<String>,
    pub recorded_at: u64,
}

pub struct ResolveDuplicatesAction {
    pub keep: ProjectionId,
    pub relays: Vec<String>,
    pub recorded_at: u64,
}

pub struct BigStickAction<'a> {
    pub projection: ProjectionId,
    pub portable_link: &'a str,
    pub replication_threshold: usize,
    pub archive_level: PreservationLevel,
    pub relays: Vec<String>,
    pub recorded_at: u64,
}

pub struct WithdrawalAction<'a> {
    pub projection: ProjectionId,
    pub marker: crate::WithdrawalMarker<'a>,
    pub replication_threshold: usize,
    pub archive_level: PreservationLevel,
    pub relays: Vec<String>,
    pub recorded_at: u64,
}

impl<S: SecretStore, A: RedditAdapter> BridgeService<S, A> {
    #[must_use]
    pub fn new(secrets: S, adapter: A) -> Self {
        Self {
            projections: ProjectionService::new(secrets),
            adapter,
        }
    }

    /// Durably queues a post projection without contacting Reddit.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing or non-post object, rendering failure, or
    /// durable projection-record failure.
    pub fn queue_post(
        &self,
        store: &mut DurableStore,
        request: QueuePost<'_>,
    ) -> Result<Projection, BridgeError> {
        let head = projected_head(store, request.persona, request.anchor, ObjectKind::Post)?;
        let (rendered, losses) = render_markdown(head.body.as_str(), request.attribution)?;
        let projection = Projection {
            id: ProjectionId::new(),
            anchor: request.anchor.clone(),
            destination: ExternalId::new("reddit-community", request.subreddit)
                .map_err(AppError::Domain)?,
            external_id: None,
            external_url: None,
            persona: request.persona,
            state: ProjectionState::Queued,
            sync_enabled: true,
            payload_hash: Some(hash(&rendered)),
            last_synced_head: None,
            rendered_payload: Some(rendered),
            rendered_suffix: request.attribution.suffix()?,
            formatting_losses: losses,
            last_attempt_at: None,
            last_success_at: None,
            divergence: None,
            display_error: None,
        };
        self.projections
            .record(store, projection, request.relays, request.recorded_at)
            .map_err(Into::into)
    }

    /// Durably queues a comment projection to one exact Reddit parent.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing or non-comment object, invalid parent,
    /// rendering failure, or durable projection-record failure.
    pub fn queue_comment(
        &self,
        store: &mut DurableStore,
        request: QueueComment<'_>,
    ) -> Result<Projection, BridgeError> {
        let head = projected_head(store, request.persona, request.anchor, ObjectKind::Comment)?;
        let (rendered, losses) = render_markdown(head.body.as_str(), request.attribution)?;
        let projection = Projection {
            id: ProjectionId::new(),
            anchor: request.anchor.clone(),
            destination: ExternalId::new("reddit-parent", request.parent.as_str())
                .map_err(AppError::Domain)?,
            external_id: None,
            external_url: None,
            persona: request.persona,
            state: ProjectionState::Queued,
            sync_enabled: true,
            payload_hash: Some(hash(&rendered)),
            last_synced_head: None,
            rendered_payload: Some(rendered),
            rendered_suffix: request.attribution.suffix()?,
            formatting_losses: losses,
            last_attempt_at: None,
            last_success_at: None,
            divergence: None,
            display_error: None,
        };
        self.projections
            .record(store, projection, request.relays, request.recorded_at)
            .map_err(Into::into)
    }

    /// Reconciles and executes one queued projection. Recent matching Reddit
    /// history is adopted before any submission, making interrupted retries
    /// idempotent without relying on Reddit idempotency support.
    ///
    /// # Errors
    ///
    /// Returns an error when journal integrity fails, the projection is not
    /// queued, the adapter rejects the operation, or state cannot be recorded.
    pub fn execute(
        &self,
        store: &mut DurableStore,
        projection_id: ProjectionId,
        relays: Vec<String>,
        recorded_at: u64,
    ) -> Result<Projection, BridgeError> {
        let mut projection = store
            .state()
            .projections
            .get(&projection_id)
            .cloned()
            .ok_or(BridgeError::NotQueued)?;
        if projection.state != ProjectionState::Queued {
            return Err(BridgeError::NotQueued);
        }
        let rendered = projection
            .rendered_payload
            .clone()
            .ok_or(BridgeError::PayloadChanged)?;
        if projection.payload_hash.as_deref() != Some(hash(&rendered).as_str()) {
            return Err(BridgeError::PayloadChanged);
        }
        let head = store
            .state()
            .heads
            .current_head(&projection.anchor)
            .ok()
            .cloned()
            .ok_or(BridgeError::InvalidDestination)?;

        projection
            .transition(ProjectionState::Submitting)
            .map_err(AppError::Domain)?;
        projection.last_attempt_at = Some(recorded_at);
        projection.display_error = None;
        self.projections
            .record(store, projection.clone(), relays.clone(), recorded_at)?;

        let result = self
            .find_existing(&projection, &head, &rendered)
            .and_then(|existing| {
                existing.map_or_else(|| self.submit(&projection, &head, &rendered), Ok)
            });
        match result {
            Ok(thing) => {
                projection.external_id = Some(
                    ExternalId::new("reddit", thing.fullname.as_str()).map_err(AppError::Domain)?,
                );
                projection.external_url = Some(absolute_permalink(&thing.permalink));
                projection.last_synced_head = Some(head_revision(&head));
                projection.last_success_at = Some(recorded_at);
                projection
                    .transition(ProjectionState::Live)
                    .map_err(AppError::Domain)?;
                self.projections
                    .record(store, projection, relays, recorded_at)
                    .map_err(Into::into)
            }
            Err(error) => {
                projection.display_error = Some(error.to_string());
                let failure = if matches!(error, RedditError::Rejected(_)) {
                    ProjectionState::Rejected
                } else {
                    ProjectionState::Failed
                };
                projection.transition(failure).map_err(AppError::Domain)?;
                self.projections
                    .record(store, projection, relays, recorded_at)?;
                Err(error.into())
            }
        }
    }

    /// Keeps one projection as the local canonical mapping and abandons other
    /// active mappings for the same persona, Hydra object, and destination.
    /// Existing Reddit objects are not deleted or edited by this local choice.
    ///
    /// # Errors
    ///
    /// Returns an error when the kept projection is missing or no duplicate
    /// group exists.
    pub fn resolve_duplicates(
        &self,
        store: &mut DurableStore,
        action: &ResolveDuplicatesAction,
    ) -> Result<Vec<Projection>, BridgeError> {
        let keep = stored_projection(store, action.keep)?;
        let candidates = store
            .state()
            .projections
            .values()
            .filter(|projection| {
                projection.persona == keep.persona
                    && projection.anchor == keep.anchor
                    && projection.destination == keep.destination
                    && !matches!(
                        projection.state,
                        ProjectionState::Abandoned | ProjectionState::Withdrawn
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        if candidates.len() < 2 {
            return Err(BridgeError::InvalidDestination);
        }
        let mut resolved = Vec::new();
        for mut projection in candidates {
            if projection.id == action.keep {
                resolved.push(projection);
                continue;
            }
            projection
                .transition(ProjectionState::Abandoned)
                .map_err(AppError::Domain)?;
            projection.sync_enabled = false;
            projection.display_error = Some(format!(
                "Duplicate mapping resolved in favor of projection {}. Any existing Reddit object was left untouched.",
                action.keep
            ));
            self.projections.record(
                store,
                projection.clone(),
                action.relays.clone(),
                action.recorded_at,
            )?;
            resolved.push(projection);
        }
        Ok(resolved)
    }

    /// Refreshes one live Reddit projection and records removal, locking, or
    /// text divergence without replacing the canonical Hydra head.
    ///
    /// # Errors
    ///
    /// Returns an error when the projection has no exact Reddit object, cannot
    /// enter synchronization, or the adapter/state journal fails.
    pub fn synchronize(
        &self,
        store: &mut DurableStore,
        action: ProjectionAction,
    ) -> Result<Projection, BridgeError> {
        let mut projection = stored_projection(store, action.projection)?;
        let fullname = projection_fullname(&projection)?;
        projection
            .transition(ProjectionState::Synchronizing)
            .map_err(AppError::Domain)?;
        projection.last_attempt_at = Some(action.recorded_at);
        self.projections.record(
            store,
            projection.clone(),
            action.relays.clone(),
            action.recorded_at,
        )?;

        match self.adapter.fetch(&fullname) {
            Ok(thing) => {
                projection.external_url = Some(absolute_permalink(&thing.permalink));
                projection.last_success_at = Some(action.recorded_at);
                if thing.removed {
                    projection
                        .transition(ProjectionState::Removed)
                        .map_err(AppError::Domain)?;
                } else if hash(&thing.body) != projection.payload_hash.clone().unwrap_or_default() {
                    projection.divergence = Some(thing.body);
                    projection
                        .transition(ProjectionState::Diverged)
                        .map_err(AppError::Domain)?;
                } else if thing.locked {
                    projection
                        .transition(ProjectionState::Locked)
                        .map_err(AppError::Domain)?;
                } else {
                    projection.divergence = None;
                    projection
                        .transition(ProjectionState::Live)
                        .map_err(AppError::Domain)?;
                }
                self.projections
                    .record(store, projection, action.relays, action.recorded_at)
                    .map_err(Into::into)
            }
            Err(RedditError::Unavailable) => {
                projection
                    .transition(ProjectionState::Removed)
                    .map_err(AppError::Domain)?;
                self.projections
                    .record(store, projection, action.relays, action.recorded_at)
                    .map_err(Into::into)
            }
            Err(error) => {
                projection.display_error = Some(error.to_string());
                projection
                    .transition(ProjectionState::Failed)
                    .map_err(AppError::Domain)?;
                self.projections
                    .record(store, projection, action.relays, action.recorded_at)?;
                Err(error.into())
            }
        }
    }

    /// Appends Big Stick's exact portable record link only after both the
    /// immutable anchor and latest editable head satisfy relay replication.
    ///
    /// # Errors
    ///
    /// Returns an error when preservation is insufficient, the projection is
    /// inactive, rendering fails, or Reddit/state synchronization fails.
    pub fn attach_big_stick(
        &self,
        store: &mut DurableStore,
        action: BigStickAction<'_>,
    ) -> Result<Projection, BridgeError> {
        let projection = stored_projection(store, action.projection)?;
        let operation = OperationId::new();
        record_continuity(
            store,
            operation,
            projection.persona,
            action.projection.to_string(),
            ContinuityState::BigStick(BigStickState::Requested),
            action.recorded_at,
        )?;
        for state in [BigStickState::IdentifyingObject, BigStickState::Archiving] {
            transition_continuity(
                store,
                operation,
                ContinuityState::BigStick(state),
                action.recorded_at,
            )?;
        }
        record_native_archive::<S>(
            store,
            projection.persona,
            &projection.anchor,
            action.archive_level,
            action.recorded_at,
        )?;
        transition_continuity(
            store,
            operation,
            ContinuityState::BigStick(BigStickState::Verifying),
            action.recorded_at,
        )?;
        if let Err(error) =
            require_replication(store, &projection.anchor, action.replication_threshold)
        {
            transition_continuity(
                store,
                operation,
                ContinuityState::BigStick(BigStickState::ArchiveFailed),
                action.recorded_at,
            )?;
            return Err(error);
        }
        transition_continuity(
            store,
            operation,
            ContinuityState::BigStick(BigStickState::EditingReddit),
            action.recorded_at,
        )?;
        let result = self.update_reddit_text(
            store,
            projection,
            Attribution::BigStick(action.portable_link),
            action.relays,
            action.recorded_at,
        );
        transition_continuity(
            store,
            operation,
            ContinuityState::BigStick(if result.is_ok() {
                BigStickState::Complete
            } else {
                BigStickState::EditFailed
            }),
            action.recorded_at,
        )?;
        result
    }

    /// Replaces one Reddit projection with the selected Reddacted marker only
    /// after verified relay replication, then makes withdrawal sticky.
    ///
    /// # Errors
    ///
    /// Returns an error when preservation is insufficient, the projection is
    /// unavailable, marker rendering fails, or Reddit/state synchronization
    /// fails.
    pub fn withdraw(
        &self,
        store: &mut DurableStore,
        action: WithdrawalAction<'_>,
    ) -> Result<Projection, BridgeError> {
        let mut projection = stored_projection(store, action.projection)?;
        let operation = OperationId::new();
        record_continuity(
            store,
            operation,
            projection.persona,
            action.projection.to_string(),
            ContinuityState::Reddacted(ReddactedState::Requested),
            action.recorded_at,
        )?;
        for state in [ReddactedState::Previewed, ReddactedState::Archiving] {
            transition_continuity(
                store,
                operation,
                ContinuityState::Reddacted(state),
                action.recorded_at,
            )?;
        }
        record_native_archive::<S>(
            store,
            projection.persona,
            &projection.anchor,
            action.archive_level,
            action.recorded_at,
        )?;
        if let Err(error) =
            require_replication(store, &projection.anchor, action.replication_threshold)
        {
            transition_continuity(
                store,
                operation,
                ContinuityState::Reddacted(ReddactedState::Failed),
                action.recorded_at,
            )?;
            return Err(error);
        }
        transition_continuity(
            store,
            operation,
            ContinuityState::Reddacted(ReddactedState::Verified),
            action.recorded_at,
        )?;
        let fullname = projection_fullname(&projection)?;
        let marker = crate::withdrawal_marker(action.marker)?;
        transition_continuity(
            store,
            operation,
            ContinuityState::Reddacted(ReddactedState::Withdrawing),
            action.recorded_at,
        )?;
        if let Err(error) = self.adapter.edit(&fullname, &marker) {
            transition_continuity(
                store,
                operation,
                ContinuityState::Reddacted(ReddactedState::Failed),
                action.recorded_at,
            )?;
            return Err(error.into());
        }
        projection.rendered_payload = Some(marker.clone());
        projection.rendered_suffix = None;
        projection.payload_hash = Some(hash(&marker));
        projection.last_success_at = Some(action.recorded_at);
        projection.divergence = None;
        projection.display_error = None;
        projection.sync_enabled = false;
        projection
            .transition(ProjectionState::Withdrawn)
            .map_err(AppError::Domain)?;
        let projection = self
            .projections
            .record(store, projection, action.relays, action.recorded_at)
            .map_err(BridgeError::from)?;
        transition_continuity(
            store,
            operation,
            ContinuityState::Reddacted(ReddactedState::Withdrawn),
            action.recorded_at,
        )?;
        Ok(projection)
    }

    fn update_reddit_text(
        &self,
        store: &mut DurableStore,
        mut projection: Projection,
        attribution: Attribution<'_>,
        relays: Vec<String>,
        recorded_at: u64,
    ) -> Result<Projection, BridgeError> {
        let fullname = projection_fullname(&projection)?;
        let head = store
            .state()
            .heads
            .current_head(&projection.anchor)
            .map_err(|_| BridgeError::InvalidDestination)?
            .clone();
        let (rendered, losses) = render_markdown(head.body.as_str(), attribution)?;
        let rendered_suffix = attribution.suffix()?;
        projection
            .transition(ProjectionState::Synchronizing)
            .map_err(AppError::Domain)?;
        projection.last_attempt_at = Some(recorded_at);
        self.projections
            .record(store, projection.clone(), relays.clone(), recorded_at)?;
        if let Err(error) = self.adapter.edit(&fullname, &rendered) {
            projection.display_error = Some(error.to_string());
            projection
                .transition(ProjectionState::Failed)
                .map_err(AppError::Domain)?;
            self.projections
                .record(store, projection, relays, recorded_at)?;
            return Err(error.into());
        }
        projection.rendered_payload = Some(rendered.clone());
        projection.rendered_suffix = rendered_suffix;
        projection.payload_hash = Some(hash(&rendered));
        projection.formatting_losses = losses;
        projection.last_synced_head = Some(head_revision(&head));
        projection.last_success_at = Some(recorded_at);
        projection.divergence = None;
        projection.display_error = None;
        projection
            .transition(ProjectionState::Live)
            .map_err(AppError::Domain)?;
        self.projections
            .record(store, projection, relays, recorded_at)
            .map_err(Into::into)
    }

    /// Pushes the latest canonical Hydra head to an existing projection while
    /// preserving its explicitly selected marker. Withdrawn content remains
    /// withdrawn until a separate restore action requeues it.
    ///
    /// # Errors
    ///
    /// Returns an error when the projection cannot be updated or is withdrawn.
    pub fn push_current(
        &self,
        store: &mut DurableStore,
        action: ProjectionAction,
    ) -> Result<Projection, BridgeError> {
        let projection = stored_projection(store, action.projection)?;
        if projection.state == ProjectionState::Withdrawn {
            return Err(BridgeError::Withdrawn);
        }
        let rendered_suffix = projection.rendered_suffix.clone();
        let attribution = rendered_suffix
            .as_deref()
            .map_or(Attribution::None, Attribution::Literal);
        self.update_reddit_text(
            store,
            projection,
            attribution,
            action.relays,
            action.recorded_at,
        )
    }

    fn find_existing(
        &self,
        projection: &Projection,
        head: &ObjectHead,
        rendered: &str,
    ) -> Result<Option<RedditThing>, RedditError> {
        let identity = self.adapter.identity()?;
        let page = self.adapter.history(&identity.username, None)?;
        Ok(page.things.into_iter().find(|thing| {
            thing.body == rendered
                && match projection.destination.system.as_str() {
                    "reddit-community" => {
                        thing
                            .subreddit
                            .eq_ignore_ascii_case(&projection.destination.canonical)
                            && thing.title == head.title
                    }
                    "reddit-parent" => thing
                        .parent
                        .as_ref()
                        .is_some_and(|parent| parent.as_str() == projection.destination.canonical),
                    _ => false,
                }
        }))
    }

    fn submit(
        &self,
        projection: &Projection,
        head: &ObjectHead,
        rendered: &str,
    ) -> Result<RedditThing, RedditError> {
        match projection.destination.system.as_str() {
            "reddit-community" if head.kind == ObjectKind::Post => {
                self.adapter.submit_post(&SubmitPost {
                    subreddit: projection.destination.canonical.clone(),
                    title: head.title.clone().unwrap_or_default(),
                    body: rendered.to_owned(),
                })
            }
            "reddit-parent" if head.kind == ObjectKind::Comment => {
                self.adapter.submit_comment(&SubmitComment {
                    parent: RedditFullname::parse(projection.destination.canonical.clone())?,
                    body: rendered.to_owned(),
                })
            }
            _ => Err(RedditError::Invalid(
                "projection destination does not match the Hydra object".to_owned(),
            )),
        }
    }
}

fn record_continuity(
    store: &mut DurableStore,
    id: OperationId,
    persona: PersonaId,
    subject: String,
    state: ContinuityState,
    recorded_at: u64,
) -> Result<(), BridgeError> {
    store
        .append(
            DurableEvent::ContinuityWorkflowChanged(ContinuityWorkflow {
                id,
                persona,
                subject: Some(subject),
                state,
            }),
            recorded_at,
        )
        .map_err(AppError::Store)?;
    Ok(())
}

fn transition_continuity(
    store: &mut DurableStore,
    id: OperationId,
    next: ContinuityState,
    recorded_at: u64,
) -> Result<(), BridgeError> {
    let mut workflow = store
        .state()
        .continuity_workflows
        .get(&id)
        .cloned()
        .ok_or_else(|| BridgeError::Credential("continuity journal is missing".to_owned()))?;
    workflow.transition(next).map_err(AppError::Domain)?;
    store
        .append(
            DurableEvent::ContinuityWorkflowChanged(workflow),
            recorded_at,
        )
        .map_err(AppError::Store)?;
    Ok(())
}

fn record_native_archive<S: SecretStore>(
    store: &mut DurableStore,
    persona: PersonaId,
    selected_anchor: &AnchorId,
    level: PreservationLevel,
    captured_at: u64,
) -> Result<(), BridgeError> {
    let selected_head = store
        .state()
        .heads
        .current_head(selected_anchor)
        .map_err(|_| BridgeError::InvalidDestination)?;
    let root = selected_head
        .root
        .as_ref()
        .unwrap_or(&selected_head.anchor)
        .clone();
    let parent = selected_head.parent.clone();
    let mut selected = vec![selected_anchor.clone()];
    if matches!(
        level,
        PreservationLevel::Ancestors
            | PreservationLevel::VisibleSiblings
            | PreservationLevel::LoadedThread
    ) {
        let mut cursor = selected_head.parent.clone();
        while let Some(anchor) = cursor {
            if selected.contains(&anchor) {
                break;
            }
            selected.push(anchor.clone());
            cursor = store
                .state()
                .heads
                .current_head(&anchor)
                .ok()
                .and_then(|head| head.parent.clone());
        }
        if !selected.contains(&root) {
            selected.push(root.clone());
        }
    }
    if matches!(
        level,
        PreservationLevel::VisibleSiblings | PreservationLevel::LoadedThread
    ) {
        selected.extend(
            store
                .state()
                .heads
                .current_heads()
                .filter(|head| head.parent == parent)
                .map(|head| head.anchor.clone()),
        );
    }
    if level == PreservationLevel::LoadedThread {
        selected.extend(
            store
                .state()
                .heads
                .current_heads()
                .filter(|head| head.anchor == root || head.root.as_ref() == Some(&root))
                .map(|head| head.anchor.clone()),
        );
    }
    selected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    selected.dedup();
    let identifiers = selected
        .iter()
        .map(|anchor| ExternalId::new("nostr", anchor.as_str()).map_err(AppError::Domain))
        .collect::<Result<Vec<_>, _>>()?;
    let subject = ExternalId::new("nostr", selected_anchor.as_str()).map_err(AppError::Domain)?;
    let media_preserved = store
        .state()
        .media
        .values()
        .filter(|manifest| selected.contains(&manifest.object))
        .map(|manifest| {
            ExternalId::new("sha256", manifest.sha256.clone()).map_err(AppError::Domain)
        })
        .collect::<Result<Vec<_>, _>>()?;
    ArchiveService::<S>::record_manifest(
        store,
        ArchiveManifest {
            id: ArchiveId::new(),
            observer: persona,
            selected: subject,
            level,
            loaded: identifiers.clone(),
            preserved: identifiers,
            media_preserved,
            media_unavailable: Vec::new(),
            captured_at,
        },
    )?;
    Ok(())
}

fn projected_head<'a>(
    store: &'a DurableStore,
    persona: PersonaId,
    anchor: &AnchorId,
    kind: ObjectKind,
) -> Result<&'a ObjectHead, BridgeError> {
    let head = store
        .state()
        .heads
        .current_head(anchor)
        .ok()
        .ok_or(BridgeError::InvalidDestination)?;
    let author = store
        .state()
        .personas
        .get(persona)
        .ok_or(BridgeError::InvalidDestination)?;
    if head.kind != kind || head.author != author.public_key || author.reddit_account.is_none() {
        return Err(BridgeError::InvalidDestination);
    }
    Ok(head)
}

fn stored_projection(
    store: &DurableStore,
    projection: ProjectionId,
) -> Result<Projection, BridgeError> {
    store
        .state()
        .projections
        .get(&projection)
        .cloned()
        .ok_or(BridgeError::InvalidDestination)
}

fn projection_fullname(projection: &Projection) -> Result<RedditFullname, BridgeError> {
    let external = projection
        .external_id
        .as_ref()
        .filter(|external| external.system == "reddit")
        .ok_or(BridgeError::InvalidDestination)?;
    RedditFullname::parse(external.canonical.clone()).map_err(Into::into)
}

pub(crate) fn require_replication(
    store: &DurableStore,
    anchor: &AnchorId,
    threshold: usize,
) -> Result<(), BridgeError> {
    if threshold == 0 {
        return Err(BridgeError::NotReplicated);
    }
    let state = store.state();
    let anchor_accepted = accepted_relays(state, anchor.as_str());
    let head_identifier = format!("hydra:head:{}", anchor.as_str());
    let latest_head = state
        .outbound
        .values()
        .filter_map(|outbound| {
            let value = serde_json::from_str::<serde_json::Value>(&outbound.event_json).ok()?;
            let matches = value.get("tags")?.as_array()?.iter().any(|tag| {
                tag.as_array().is_some_and(|parts| {
                    parts.first().and_then(serde_json::Value::as_str) == Some("d")
                        && parts.get(1).and_then(serde_json::Value::as_str)
                            == Some(head_identifier.as_str())
                })
            });
            matches.then(|| {
                (
                    value
                        .get("created_at")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or_default(),
                    outbound.event_id.as_str(),
                )
            })
        })
        .max_by_key(|(created_at, _)| *created_at)
        .map(|(_, event_id)| event_id);
    let head_missing_replication = state.heads.contains(anchor)
        && latest_head.is_none_or(|event_id| accepted_relays(state, event_id) < threshold);
    if anchor_accepted < threshold || head_missing_replication {
        return Err(BridgeError::NotReplicated);
    }
    Ok(())
}

fn accepted_relays(state: &hydra_store::ReplayState, event_id: &str) -> usize {
    state
        .deliveries
        .iter()
        .filter(|((candidate, _), delivery)| {
            candidate == event_id && **delivery == DeliveryState::Accepted
        })
        .count()
}

fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn head_revision(head: &ObjectHead) -> String {
    hash(&format!(
        "{}\n{}\n{}",
        head.title.as_deref().unwrap_or_default(),
        head.body.as_str(),
        head.edited_at
    ))
}

pub(crate) fn absolute_permalink(permalink: &str) -> String {
    if permalink.starts_with("https://") {
        permalink.to_owned()
    } else {
        format!("https://www.reddit.com{permalink}")
    }
}

fn lock_error<T>(error: &std::sync::PoisonError<T>) -> BridgeError {
    BridgeError::Credential(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        collections::BTreeMap,
        rc::Rc,
        sync::{Arc, Mutex},
    };

    use hydra_app::{CreatePost, DiscussionService, EditObject};
    use hydra_domain::{CommunityKey, DurableEvent, Persona};
    use tempfile::tempdir;

    use super::*;

    #[derive(Default, Clone)]
    struct MemorySecrets(Rc<RefCell<BTreeMap<PersonaId, String>>>);

    impl SecretStore for MemorySecrets {
        fn set(&self, persona: PersonaId, secret: &str) -> Result<(), AppError> {
            self.0.borrow_mut().insert(persona, secret.to_owned());
            Ok(())
        }

        fn get(&self, persona: PersonaId) -> Result<String, AppError> {
            self.0
                .borrow()
                .get(&persona)
                .cloned()
                .ok_or_else(|| AppError::Credential("secret not found".to_owned()))
        }

        fn delete(&self, persona: PersonaId) -> Result<(), AppError> {
            self.0.borrow_mut().remove(&persona);
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct FakeState {
        history: Vec<RedditThing>,
        submissions: usize,
        reject: bool,
    }

    #[derive(Debug, Clone)]
    struct FakeReddit {
        identity: RedditIdentity,
        state: Arc<Mutex<FakeState>>,
    }

    impl FakeReddit {
        fn alice() -> Self {
            Self {
                identity: RedditIdentity {
                    username: "alice".to_owned(),
                    account_id: "account-alice".to_owned(),
                },
                state: Arc::new(Mutex::new(FakeState::default())),
            }
        }

        fn post(body: &str) -> RedditThing {
            RedditThing {
                fullname: RedditFullname::parse("t3_abc123").unwrap(),
                author: Some("alice".to_owned()),
                subreddit: "science".to_owned(),
                title: Some("Fungal networks".to_owned()),
                body: body.to_owned(),
                permalink: "/r/science/comments/abc123/fungal_networks/".to_owned(),
                parent: None,
                locked: false,
                removed: false,
                deleted: false,
                media_urls: Vec::new(),
                edited_at: None,
                created_at: 1,
            }
        }
    }

    impl RedditAdapter for FakeReddit {
        fn identity(&self) -> Result<RedditIdentity, RedditError> {
            Ok(self.identity.clone())
        }

        fn submit_post(&self, request: &SubmitPost) -> Result<RedditThing, RedditError> {
            let mut state = self.state.lock().unwrap();
            if state.reject {
                return Err(RedditError::Rejected("subreddit rejected post".to_owned()));
            }
            state.submissions += 1;
            let mut thing = Self::post(&request.body);
            thing.title = Some(request.title.clone());
            thing.subreddit.clone_from(&request.subreddit);
            state.history.push(thing.clone());
            Ok(thing)
        }

        fn submit_comment(&self, _request: &SubmitComment) -> Result<RedditThing, RedditError> {
            Err(RedditError::Rejected("not used in this test".to_owned()))
        }

        fn edit(&self, target: &RedditFullname, body: &str) -> Result<RedditThing, RedditError> {
            let mut state = self.state.lock().unwrap();
            let thing = state
                .history
                .iter_mut()
                .find(|thing| thing.fullname == *target)
                .ok_or_else(|| RedditError::Rejected("missing".to_owned()))?;
            thing.body = body.to_owned();
            Ok(thing.clone())
        }

        fn delete(&self, _target: &RedditFullname) -> Result<(), RedditError> {
            Ok(())
        }

        fn vote(&self, _target: &RedditFullname, _direction: i8) -> Result<(), RedditError> {
            Ok(())
        }

        fn fetch(&self, target: &RedditFullname) -> Result<RedditThing, RedditError> {
            self.state
                .lock()
                .unwrap()
                .history
                .iter()
                .find(|thing| thing.fullname == *target)
                .cloned()
                .ok_or_else(|| RedditError::Rejected("missing".to_owned()))
        }

        fn history(
            &self,
            _username: &str,
            _after: Option<&str>,
        ) -> Result<crate::HistoryPage, RedditError> {
            Ok(crate::HistoryPage {
                things: self.state.lock().unwrap().history.clone(),
                after: None,
            })
        }
    }

    fn linked_post(
        store: &mut DurableStore,
        secrets: &MemorySecrets,
        reddit: &FakeReddit,
    ) -> (Persona, ObjectHead) {
        let personas = PersonaService::new(secrets.clone());
        let persona = personas.create(store, "Alice".to_owned(), 1).unwrap();
        let credential = RedditCredential {
            identity: reddit.identity.clone(),
            client_id: Some("client".to_owned()),
            access_token: "access".to_owned(),
            refresh_token: Some("refresh".to_owned()),
            expires_at: 3_600,
            scope: "identity read history submit edit vote".to_owned(),
        };
        RedditLinkService::new(MemoryRedditCredentialStore::default())
            .link(&personas, store, persona.id, reddit, &credential, 2)
            .unwrap();
        let post = DiscussionService::new(secrets.clone())
            .create_post(
                store,
                CreatePost {
                    persona_id: persona.id,
                    title: "Fungal networks".to_owned(),
                    body: "Persistent evidence".to_owned(),
                    communities: vec![CommunityKey::parse("science").unwrap()],
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 3,
                },
            )
            .unwrap();
        (persona, post)
    }

    fn replicate_object(store: &mut DurableStore, anchor: &AnchorId, recorded_at: u64) {
        let head_identifier = format!("hydra:head:{}", anchor.as_str());
        let deliveries = store
            .state()
            .outbound
            .values()
            .filter(|outbound| {
                outbound.event_id == anchor.as_str()
                    || outbound.event_json.contains(&head_identifier)
            })
            .map(|outbound| {
                (
                    outbound.event_id.clone(),
                    outbound.relays.first().unwrap().clone(),
                )
            })
            .collect::<Vec<_>>();
        for (event_id, relay) in deliveries {
            store
                .append(
                    DurableEvent::DeliveryRecorded {
                        event_id,
                        relay,
                        state: DeliveryState::Accepted,
                    },
                    recorded_at,
                )
                .unwrap();
        }
    }

    #[test]
    fn projection_is_queued_locally_then_submitted_exactly_once() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let reddit = FakeReddit::alice();
        let (persona, post) = linked_post(&mut store, &secrets, &reddit);
        let bridge = BridgeService::new(secrets, reddit.clone());
        let projection = bridge
            .queue_post(
                &mut store,
                QueuePost {
                    persona: persona.id,
                    anchor: &post.anchor,
                    subreddit: "science",
                    attribution: Attribution::None,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 4,
                },
            )
            .unwrap();
        assert_eq!(reddit.state.lock().unwrap().submissions, 0);

        let projected = bridge
            .execute(
                &mut store,
                projection.id,
                vec!["wss://relay.example".to_owned()],
                5,
            )
            .unwrap();
        assert_eq!(projected.state, ProjectionState::Live);
        assert_eq!(projected.external_id.unwrap().canonical, "t3_abc123");
        assert_eq!(reddit.state.lock().unwrap().submissions, 1);

        drop(store);
        let reopened = DurableStore::open(root.path()).unwrap();
        assert_eq!(
            reopened.state().projections[&projection.id].state,
            ProjectionState::Live
        );
    }

    #[test]
    fn duplicate_resolution_keeps_one_mapping_without_touching_reddit() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let reddit = FakeReddit::alice();
        let (persona, post) = linked_post(&mut store, &secrets, &reddit);
        let bridge = BridgeService::new(secrets, reddit.clone());
        let queue = |store: &mut DurableStore, recorded_at| {
            bridge
                .queue_post(
                    store,
                    QueuePost {
                        persona: persona.id,
                        anchor: &post.anchor,
                        subreddit: "science",
                        attribution: Attribution::None,
                        relays: vec!["wss://relay.example".to_owned()],
                        recorded_at,
                    },
                )
                .unwrap()
        };
        let keep = queue(&mut store, 4);
        let duplicate = queue(&mut store, 5);

        let resolved = bridge
            .resolve_duplicates(
                &mut store,
                &ResolveDuplicatesAction {
                    keep: keep.id,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 6,
                },
            )
            .unwrap();

        assert_eq!(resolved.len(), 2);
        assert_eq!(
            store.state().projections[&keep.id].state,
            ProjectionState::Queued
        );
        assert_eq!(
            store.state().projections[&duplicate.id].state,
            ProjectionState::Abandoned
        );
        assert!(!store.state().projections[&duplicate.id].sync_enabled);
        assert_eq!(reddit.state.lock().unwrap().submissions, 0);
    }

    #[test]
    fn retry_reconciles_matching_history_before_submitting() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let reddit = FakeReddit::alice();
        reddit
            .state
            .lock()
            .unwrap()
            .history
            .push(FakeReddit::post("Persistent evidence"));
        let (persona, post) = linked_post(&mut store, &secrets, &reddit);
        let bridge = BridgeService::new(secrets, reddit.clone());
        let projection = bridge
            .queue_post(
                &mut store,
                QueuePost {
                    persona: persona.id,
                    anchor: &post.anchor,
                    subreddit: "science",
                    attribution: Attribution::None,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 4,
                },
            )
            .unwrap();

        bridge
            .execute(
                &mut store,
                projection.id,
                vec!["wss://relay.example".to_owned()],
                5,
            )
            .unwrap();
        assert_eq!(reddit.state.lock().unwrap().submissions, 0);
    }

    #[test]
    fn reddit_rejection_is_preserved_as_a_durable_projection_state() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let reddit = FakeReddit::alice();
        reddit.state.lock().unwrap().reject = true;
        let (persona, post) = linked_post(&mut store, &secrets, &reddit);
        let bridge = BridgeService::new(secrets, reddit);
        let projection = bridge
            .queue_post(
                &mut store,
                QueuePost {
                    persona: persona.id,
                    anchor: &post.anchor,
                    subreddit: "science",
                    attribution: Attribution::None,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 4,
                },
            )
            .unwrap();

        assert!(
            bridge
                .execute(
                    &mut store,
                    projection.id,
                    vec!["wss://relay.example".to_owned()],
                    5,
                )
                .is_err()
        );
        assert_eq!(
            store.state().projections[&projection.id].state,
            ProjectionState::Rejected
        );
    }

    #[test]
    fn synchronization_records_divergence_without_replacing_hydra() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let reddit = FakeReddit::alice();
        let (persona, post) = linked_post(&mut store, &secrets, &reddit);
        let bridge = BridgeService::new(secrets, reddit.clone());
        let projection = bridge
            .queue_post(
                &mut store,
                QueuePost {
                    persona: persona.id,
                    anchor: &post.anchor,
                    subreddit: "science",
                    attribution: Attribution::None,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 4,
                },
            )
            .unwrap();
        bridge
            .execute(
                &mut store,
                projection.id,
                vec!["wss://relay.example".to_owned()],
                5,
            )
            .unwrap();
        reddit.state.lock().unwrap().history[0].body = "Edited on Reddit".to_owned();

        let synced = bridge
            .synchronize(
                &mut store,
                ProjectionAction {
                    projection: projection.id,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 6,
                },
            )
            .unwrap();
        assert_eq!(synced.state, ProjectionState::Diverged);
        assert_eq!(synced.divergence.as_deref(), Some("Edited on Reddit"));
        assert_eq!(
            store
                .state()
                .heads
                .current_head(&post.anchor)
                .unwrap()
                .body
                .as_str(),
            "Persistent evidence"
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn big_stick_and_withdrawal_require_verified_replication() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let reddit = FakeReddit::alice();
        let (persona, post) = linked_post(&mut store, &secrets, &reddit);
        let bridge = BridgeService::new(secrets, reddit.clone());
        let projection = bridge
            .queue_post(
                &mut store,
                QueuePost {
                    persona: persona.id,
                    anchor: &post.anchor,
                    subreddit: "science",
                    attribution: Attribution::None,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 4,
                },
            )
            .unwrap();
        bridge
            .execute(
                &mut store,
                projection.id,
                vec!["wss://relay.example".to_owned()],
                5,
            )
            .unwrap();

        assert!(matches!(
            bridge.attach_big_stick(
                &mut store,
                BigStickAction {
                    projection: projection.id,
                    portable_link: "nostr:nevent1example",
                    replication_threshold: 1,
                    archive_level: PreservationLevel::Item,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 6,
                }
            ),
            Err(BridgeError::NotReplicated)
        ));
        assert!(store.state().continuity_workflows.values().any(|workflow| {
            workflow.state == ContinuityState::BigStick(BigStickState::ArchiveFailed)
        }));
        replicate_object(&mut store, &post.anchor, 7);
        bridge
            .attach_big_stick(
                &mut store,
                BigStickAction {
                    projection: projection.id,
                    portable_link: "nostr:nevent1example",
                    replication_threshold: 1,
                    archive_level: PreservationLevel::Item,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 8,
                },
            )
            .unwrap();
        assert!(store.state().continuity_workflows.values().any(|workflow| {
            workflow.state == ContinuityState::BigStick(BigStickState::Complete)
        }));
        assert!(
            store
                .state()
                .archive_manifests
                .values()
                .all(|manifest| manifest.level == PreservationLevel::Item)
        );
        assert!(
            reddit.state.lock().unwrap().history[0]
                .body
                .contains("Uncensorable record")
        );

        let withdrawn = bridge
            .withdraw(
                &mut store,
                WithdrawalAction {
                    projection: projection.id,
                    marker: crate::WithdrawalMarker::Withdrawn("nostr:nevent1example"),
                    replication_threshold: 1,
                    archive_level: PreservationLevel::Item,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 9,
                },
            )
            .unwrap();
        assert_eq!(withdrawn.state, ProjectionState::Withdrawn);
        assert!(store.state().continuity_workflows.values().any(|workflow| {
            workflow.state == ContinuityState::Reddacted(ReddactedState::Withdrawn)
        }));
        assert!(
            reddit.state.lock().unwrap().history[0]
                .body
                .contains("Withdrawn from Reddit")
        );
        assert!(
            bridge
                .attach_big_stick(
                    &mut store,
                    BigStickAction {
                        projection: projection.id,
                        portable_link: "nostr:nevent1example",
                        replication_threshold: 1,
                        archive_level: PreservationLevel::Item,
                        relays: vec!["wss://relay.example".to_owned()],
                        recorded_at: 10,
                    },
                )
                .is_err()
        );
    }

    #[test]
    fn withdrawn_content_is_terminal_and_stays_out_of_auto_sync() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let reddit = FakeReddit::alice();
        let (persona, post) = linked_post(&mut store, &secrets, &reddit);
        let bridge = BridgeService::new(secrets, reddit.clone());
        let projection = bridge
            .queue_post(
                &mut store,
                QueuePost {
                    persona: persona.id,
                    anchor: &post.anchor,
                    subreddit: "science",
                    attribution: Attribution::None,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 4,
                },
            )
            .unwrap();
        bridge
            .execute(
                &mut store,
                projection.id,
                vec!["wss://relay.example".to_owned()],
                5,
            )
            .unwrap();
        replicate_object(&mut store, &post.anchor, 6);
        bridge
            .withdraw(
                &mut store,
                WithdrawalAction {
                    projection: projection.id,
                    marker: crate::WithdrawalMarker::Withdrawn("nostr:nevent1example"),
                    replication_threshold: 1,
                    archive_level: PreservationLevel::Item,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 7,
                },
            )
            .unwrap();

        let withdrawn = store.state().projections.get(&projection.id).unwrap();
        assert_eq!(withdrawn.state, ProjectionState::Withdrawn);
        assert!(!withdrawn.sync_enabled);
        assert!(
            reddit.state.lock().unwrap().history[0]
                .body
                .contains("Withdrawn from Reddit")
        );
    }

    #[test]
    fn canonical_edits_preserve_opt_in_projection_marker() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let reddit = FakeReddit::alice();
        let (persona, post) = linked_post(&mut store, &secrets, &reddit);
        let bridge = BridgeService::new(secrets.clone(), reddit.clone());
        let projection = bridge
            .queue_post(
                &mut store,
                QueuePost {
                    persona: persona.id,
                    anchor: &post.anchor,
                    subreddit: "science",
                    attribution: Attribution::PostedFromHydra("hydra://event/example"),
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 4,
                },
            )
            .unwrap();
        bridge
            .execute(
                &mut store,
                projection.id,
                vec!["wss://relay.example".to_owned()],
                5,
            )
            .unwrap();
        DiscussionService::new(secrets)
            .edit_object(
                &mut store,
                EditObject {
                    persona_id: persona.id,
                    anchor: post.anchor,
                    title: Some("Fungal networks".to_owned()),
                    body: "Revised evidence".to_owned(),
                    communities: None,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 6,
                },
            )
            .unwrap();

        let updated = bridge
            .push_current(
                &mut store,
                ProjectionAction {
                    projection: projection.id,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 7,
                },
            )
            .unwrap();
        assert_eq!(updated.state, ProjectionState::Live);
        assert_eq!(
            reddit.state.lock().unwrap().history[0].body,
            "Revised evidence\n\n[Posted from Hydra](hydra://event/example)"
        );
    }
}
