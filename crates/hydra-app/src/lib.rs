#![forbid(unsafe_code)]
//! Application services coordinate domain ports without depending on UI or Reddit.

mod backup;

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

pub use hydra_domain::PrivateState;
use hydra_domain::{
    AnchorId, ArchiveManifest, BlockRecord, CommunityAppearanceRecord, CommunityColorChoice,
    CommunityColorChoiceRecord, CommunityColorScheme, CommunityKey, CommunitySubscription,
    ContentBody, DeliveryState, DirectMessageRecord, DomainError, DraftRecord, DurableEvent,
    EncryptedPrivateRecord, ExternalId, FlairText, FlockingJudgmentRecord, FlockingProfile,
    FollowRecord, LocalFilterKind, LocalFilterRecord, MediaManifest, MessageDirection,
    NostrPublicKey, ObjectHead, ObjectKind, OutboundEvent, Persona, PersonaId, PersonaProfile,
    PostFlairChoice, PostFlairScope, PrivateRecord, Projection, PublicFollowSet,
    PublicProjectionRecord, ReactionRecord, ReactionValue, RevisitIntent, RevisitRecord,
};
pub use hydra_lens::{FeedLens, FeedService};
use hydra_media::{BlossomClient, MediaStore};
use hydra_nostr::{
    self, CommentScope, EventPublisher, EventReference, EventSigner, ExternalCommentScope,
    HydraSigner,
};
use hydra_store::{
    DurableStore, HeadStore, LocalCredentialVault, StoreError, try_platform_keyring,
};
use keyring::Entry;
use nostr::{
    Event, EventBuilder, EventId as NostrEventId, JsonUtil, Keys, Kind, NostrSigner, PublicKey,
    SignerError, ToBech32, UnsignedEvent,
};
use nostr_connect::prelude::{NostrConnect, NostrConnectURI};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use backup::BackupService;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PersonaCredential {
    Local {
        signing_secret: String,
    },
    Remote {
        bunker_uri: String,
        client_secret: String,
        user_public_key: String,
        storage_secret: String,
    },
}

impl PersonaCredential {
    pub(crate) fn decode(value: &str) -> Result<Self, AppError> {
        if value.trim_start().starts_with('{') {
            serde_json::from_str(value).map_err(|error| AppError::Credential(error.to_string()))
        } else {
            // Backward-compatible with 1.0 prerelease credentials.
            Ok(Self::Local {
                signing_secret: value.to_owned(),
            })
        }
    }

    fn encode(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(AppError::PrivateRecordEncoding)
    }

    pub(crate) fn public_key(&self) -> Result<PublicKey, AppError> {
        match self {
            Self::Local { signing_secret } => Keys::parse(signing_secret)
                .map(|keys| keys.public_key())
                .map_err(|error| AppError::KeyEncoding(error.to_string())),
            Self::Remote {
                user_public_key, ..
            } => PublicKey::parse(user_public_key)
                .map_err(|error| AppError::KeyEncoding(error.to_string())),
        }
    }
}

#[derive(Debug, Clone)]
struct PersonaSigningContext {
    signer: HydraSigner,
    storage_keys: Keys,
}

impl EventSigner for PersonaSigningContext {
    fn public_key(&self) -> PublicKey {
        self.signer.public_key()
    }

    fn sign(&self, builder: EventBuilder) -> Result<Event, hydra_nostr::ProtocolError> {
        self.signer.sign(builder)
    }
}

impl NostrSigner for PersonaSigningContext {
    fn backend(&self) -> nostr::signer::SignerBackend<'_> {
        self.signer.backend()
    }

    fn get_public_key(&self) -> nostr::util::BoxedFuture<'_, Result<PublicKey, SignerError>> {
        self.signer.get_public_key()
    }

    fn sign_event(
        &self,
        unsigned: UnsignedEvent,
    ) -> nostr::util::BoxedFuture<'_, Result<Event, SignerError>> {
        self.signer.sign_event(unsigned)
    }

    fn nip04_encrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> nostr::util::BoxedFuture<'a, Result<String, SignerError>> {
        self.signer.nip04_encrypt(public_key, content)
    }

    fn nip04_decrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> nostr::util::BoxedFuture<'a, Result<String, SignerError>> {
        self.signer.nip04_decrypt(public_key, content)
    }

    fn nip44_encrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> nostr::util::BoxedFuture<'a, Result<String, SignerError>> {
        self.signer.nip44_encrypt(public_key, content)
    }

    fn nip44_decrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        payload: &'a str,
    ) -> nostr::util::BoxedFuture<'a, Result<String, SignerError>> {
        self.signer.nip44_decrypt(public_key, payload)
    }
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("credential vault failed: {0}")]
    Credential(String),
    #[error("Nostr key encoding failed: {0}")]
    KeyEncoding(String),
    #[error(transparent)]
    Protocol(#[from] hydra_nostr::ProtocolError),
    #[error("private record encoding failed: {0}")]
    PrivateRecordEncoding(#[from] serde_json::Error),
    #[error("stored Nostr key does not belong to the selected persona")]
    PersonaKeyMismatch,
    #[error("only the authoring persona can edit this object")]
    NotObjectAuthor,
    #[error("backup failed: {0}")]
    Backup(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Flocking failed: {0}")]
    Flocking(String),
}

pub trait SecretStore {
    /// Stores the persona's Nostr secret.
    ///
    /// # Errors
    ///
    /// Returns an error when the credential store rejects the write.
    fn set(&self, persona: PersonaId, secret: &str) -> Result<(), AppError>;

    /// Loads the persona's Nostr secret.
    ///
    /// # Errors
    ///
    /// Returns an error when the credential store cannot supply the secret.
    fn get(&self, persona: PersonaId) -> Result<String, AppError>;

    /// Deletes the persona's Nostr secret.
    ///
    /// # Errors
    ///
    /// Returns an error when the credential store rejects the deletion.
    fn delete(&self, persona: PersonaId) -> Result<(), AppError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformSecretStore;

impl PlatformSecretStore {
    const SERVICE: &'static str = "org.hydra.desktop.nostr";

    fn entry(persona: PersonaId) -> Result<Entry, AppError> {
        Entry::new(Self::SERVICE, &persona.to_string())
            .map_err(|error| AppError::Credential(error.to_string()))
    }

    fn local() -> Result<LocalCredentialVault, AppError> {
        LocalCredentialVault::for_hydra("nostr")
            .map_err(|error| AppError::Credential(error.to_string()))
    }

    fn cache() -> &'static Mutex<BTreeMap<String, String>> {
        static CACHE: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
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
        "credential unavailable"
    }

    #[must_use]
    pub fn available_without_keyring(persona: PersonaId) -> bool {
        Self::local().is_ok_and(|vault| vault.get(&persona.to_string()).is_ok())
            || Self::cache_key(persona).is_some_and(|key| {
                Self::cache()
                    .lock()
                    .is_ok_and(|cache| cache.contains_key(&key))
            })
    }
}

impl SecretStore for PlatformSecretStore {
    fn set(&self, persona: PersonaId, secret: &str) -> Result<(), AppError> {
        Self::local()?
            .set(&persona.to_string(), secret)
            .map_err(|error| AppError::Credential(error.to_string()))?;
        if let Some(key) = Self::cache_key(persona) {
            Self::cache()
                .lock()
                .map_err(|error| AppError::Credential(error.to_string()))?
                .insert(key, secret.to_owned());
        }
        Ok(())
    }

    fn get(&self, persona: PersonaId) -> Result<String, AppError> {
        let cache_key = Self::cache_key(persona);
        if let Some(secret) = cache_key
            .as_ref()
            .and_then(|key| Self::cache().lock().ok()?.get(key).cloned())
        {
            return Ok(secret);
        }
        if let Ok(secret) = Self::local()?.get(&persona.to_string()) {
            if let Some(key) = cache_key {
                Self::cache()
                    .lock()
                    .map_err(|error| AppError::Credential(error.to_string()))?
                    .insert(key, secret.clone());
            }
            return Ok(secret);
        }
        let secret = try_platform_keyring(move || Self::entry(persona).ok()?.get_password().ok())
            .ok_or_else(|| AppError::Credential("credential not found".to_owned()))?;
        Self::local()?
            .set(&persona.to_string(), &secret)
            .map_err(|error| AppError::Credential(error.to_string()))?;
        if let Some(key) = cache_key {
            Self::cache()
                .lock()
                .map_err(|error| AppError::Credential(error.to_string()))?
                .insert(key, secret.clone());
        }
        Ok(secret)
    }

    fn delete(&self, persona: PersonaId) -> Result<(), AppError> {
        let local = Self::local()?;
        let local_deleted = local.delete(&persona.to_string()).is_ok();
        let keyring_deleted =
            try_platform_keyring(move || Self::entry(persona).ok()?.delete_credential().ok())
                .is_some();
        if keyring_deleted || local_deleted {
            if let Some(key) = Self::cache_key(persona) {
                Self::cache()
                    .lock()
                    .map_err(|error| AppError::Credential(error.to_string()))?
                    .remove(&key);
            }
            Ok(())
        } else {
            Err(AppError::Credential("credential not found".to_owned()))
        }
    }
}

pub struct PersonaService<S> {
    secrets: S,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ImportService;

impl ImportService {
    /// Verifies and stores one public Nostr event, materializing Hydra discussion
    /// anchors and editable heads when the event belongs to the Hydra protocol.
    /// Events may arrive out of order; a later anchor replays previously stored
    /// addressable heads for the same object. A bounded, signature-valid event
    /// with malformed required fields or a future schema is retained as raw
    /// evidence but never materialized by a client that cannot interpret it.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid event JSON/signatures, malformed Hydra
    /// discussion data, or durable append failure.
    pub fn receive_public(
        store: &mut DurableStore,
        event_json: &str,
        recorded_at: u64,
    ) -> Result<Vec<ObjectHead>, AppError> {
        if event_json.len() > OutboundEvent::MAX_EVENT_BYTES {
            return Err(DomainError::TooLong {
                max: OutboundEvent::MAX_EVENT_BYTES,
            }
            .into());
        }
        let event = nostr::Event::from_json(event_json)
            .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?;
        if !matches!(
            event.kind,
            Kind::Metadata
                | Kind::ContactList
                | Kind::TextNote
                | Kind::Thread
                | Kind::Comment
                | Kind::LongFormTextNote
                | Kind::Custom(20..=22 | 10_000)
                | Kind::Repost
                | Kind::GenericRepost
                | Kind::Reaction
                | Kind::EventDeletion
                | Kind::Label
        ) && event.kind != Kind::Custom(hydra_nostr::OBJECT_HEAD_KIND)
            && event.kind != Kind::Custom(hydra_nostr::PROJECTION_RECORD_KIND)
            && event.kind != Kind::Custom(flocking_core::JUDGMENT_KIND)
            && event.kind != Kind::Custom(flocking_core::COMMUNITY_APPEARANCE_KIND)
            && event.kind != Kind::Custom(hydra_nostr::COMMUNITY_COLOR_SCHEME_KIND)
            && event.kind != Kind::Custom(hydra_nostr::POST_FLAIR_CHOICE_KIND)
            && !hydra_nostr::is_reading_surface_event(&event)
        {
            return Ok(Vec::new());
        }
        validate_public_event_shape(&event)?;
        event
            .verify()
            .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?;
        let event_id = event.id.to_hex();
        if store.state().received_events.contains_key(&event_id) {
            return Ok(Vec::new());
        }
        retain_remote_deletion(store, &event)?;
        let materialized = match materialize_public_event(store, &event) {
            Ok(materialized) => materialized,
            Err(error) => {
                store.append(
                    DurableEvent::RemoteEventReceived {
                        event_id,
                        event_json: event_json.to_owned(),
                        heads: Vec::new(),
                        reactions: Vec::new(),
                        public_projections: Vec::new(),
                        flocking_judgments: Vec::new(),
                        community_appearances: Vec::new(),
                        community_color_choices: Vec::new(),
                        persona_profiles: Vec::new(),
                        post_flair_choices: Vec::new(),
                    },
                    recorded_at,
                )?;
                return Err(error);
            }
        };
        store.append(
            DurableEvent::RemoteEventReceived {
                event_id,
                event_json: event_json.to_owned(),
                heads: materialized.heads.clone(),
                reactions: materialized.reactions,
                public_projections: materialized.public_projections,
                flocking_judgments: materialized.flocking_judgments,
                community_appearances: materialized.community_appearances,
                community_color_choices: materialized.community_color_choices,
                persona_profiles: materialized.persona_profiles,
                post_flair_choices: materialized.post_flair_choices,
            },
            recorded_at,
        )?;
        Ok(materialized.heads)
    }
}

fn retain_remote_deletion(store: &DurableStore, event: &Event) -> Result<(), AppError> {
    if event.kind != Kind::EventDeletion {
        return Ok(());
    }
    for (tag_name, target) in event.tags.iter().filter_map(|tag| {
        let parts = tag.as_slice();
        matches!(parts.first().map(String::as_str), Some("e" | "a"))
            .then(|| parts.first().cloned().zip(parts.get(1).cloned()))
            .flatten()
    }) {
        let target = if tag_name == "a" {
            format!("nostr-address:{target}")
        } else {
            target
        };
        store.content_library().record_tombstone(
            &target,
            &event.pubkey.to_hex(),
            &event.content,
            event.created_at.as_secs(),
        )?;
        if tag_name == "e" {
            store.content_library().record_tombstone(
                &format!("nostr:{target}"),
                &event.pubkey.to_hex(),
                &event.content,
                event.created_at.as_secs(),
            )?;
        }
    }
    Ok(())
}

struct PublicMaterialization {
    heads: Vec<ObjectHead>,
    reactions: Vec<ReactionRecord>,
    public_projections: Vec<PublicProjectionRecord>,
    flocking_judgments: Vec<flocking_core::Judgment>,
    community_appearances: Vec<flocking_core::CommunityAppearance>,
    community_color_choices: Vec<CommunityColorChoice>,
    persona_profiles: Vec<PersonaProfile>,
    post_flair_choices: Vec<PostFlairChoice>,
}

fn materialize_public_event(
    store: &DurableStore,
    event: &Event,
) -> Result<PublicMaterialization, AppError> {
    let _canon = hydra_nostr::received_canon_record(event)?;
    let heads = received_heads(store, event)?;
    let reactions = remote_reactions(store, event, &heads)?;
    let mut public_projections = Vec::new();
    let flocking_judgments = hydra_nostr::received_flocking_judgments(event)?;
    let community_appearances = hydra_nostr::received_community_appearances(event)?;
    let community_color_choices = hydra_nostr::received_community_color_choices(event)?;
    let persona_profiles = hydra_nostr::received_persona_profile(event)?
        .into_iter()
        .collect();
    let post_flair_choices = hydra_nostr::received_post_flair_choices(event)?;
    if let Some(projection) = hydra_nostr::received_projection_record(event)? {
        match projection_anchor_author(store, event, &heads, &projection)? {
            Some(author) if author == projection.author => public_projections.push(projection),
            Some(_) => {
                return Err(hydra_nostr::ProtocolError::Nostr(
                    "projection signer does not own its Hydra anchor".to_owned(),
                )
                .into());
            }
            None => {}
        }
    }
    if matches!(
        event.kind,
        Kind::TextNote
            | Kind::Thread
            | Kind::Comment
            | Kind::LongFormTextNote
            | Kind::Custom(20..=22)
    ) {
        collect_waiting_projections(store, event, &heads, &mut public_projections)?;
    }
    Ok(PublicMaterialization {
        heads,
        reactions,
        public_projections,
        flocking_judgments,
        community_appearances,
        community_color_choices,
        persona_profiles,
        post_flair_choices,
    })
}

fn received_heads(store: &DurableStore, event: &Event) -> Result<Vec<ObjectHead>, AppError> {
    if event.kind == Kind::Custom(hydra_nostr::OBJECT_HEAD_KIND) {
        let mut anchors = event.tags.iter().filter_map(|tag| {
            (tag.as_slice().first().map(String::as_str) == Some("e"))
                .then(|| tag.as_slice().get(1).map(String::as_str))
                .flatten()
        });
        let anchor = anchors.next().ok_or(DomainError::InvalidObjectShape)?;
        if anchors.next().is_some() {
            return Err(DomainError::InvalidObjectShape.into());
        }
        let Some(current) = store
            .state()
            .heads
            .current_heads()
            .find(|head| head.anchor.as_str() == anchor)
        else {
            return Ok(Vec::new());
        };
        return Ok(hydra_nostr::received_object_head(event, Some(current))?
            .into_iter()
            .collect());
    }
    let Some(head) = hydra_nostr::received_object_head(event, None)? else {
        return Ok(Vec::new());
    };
    let mut heads = vec![head.clone()];
    for stored in store.state().received_events.values() {
        let stored = nostr::Event::from_json(stored)
            .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?;
        if stored.kind == Kind::Custom(hydra_nostr::OBJECT_HEAD_KIND)
            && let Ok(Some(revision)) = hydra_nostr::received_object_head(&stored, Some(&head))
        {
            heads.push(revision);
        }
    }
    Ok(heads)
}

fn collect_waiting_projections(
    store: &DurableStore,
    event: &Event,
    heads: &[ObjectHead],
    projections: &mut Vec<PublicProjectionRecord>,
) -> Result<(), AppError> {
    for stored in store.state().received_events.values() {
        let stored = nostr::Event::from_json(stored)
            .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?;
        let Ok(Some(projection)) = hydra_nostr::received_projection_record(&stored) else {
            continue;
        };
        if projection.anchor.as_str() == event.id.to_hex()
            && projection_anchor_author(store, event, heads, &projection)?
                .is_some_and(|author| author == projection.author)
        {
            projections.push(projection);
        }
    }
    Ok(())
}

fn remote_reactions(
    store: &DurableStore,
    event: &nostr::Event,
    incoming_heads: &[ObjectHead],
) -> Result<Vec<ReactionRecord>, AppError> {
    let known_targets = store
        .state()
        .heads
        .current_heads()
        .map(|head| head.anchor.clone())
        .chain(incoming_heads.iter().map(|head| head.anchor.clone()))
        .collect::<BTreeSet<_>>();
    let known_events = store
        .state()
        .reactions
        .iter()
        .map(|reaction| reaction.source_event_id.clone())
        .collect::<BTreeSet<_>>();
    let mut reactions = store
        .state()
        .received_events
        .values()
        .filter_map(|json| nostr::Event::from_json(json).ok())
        .filter_map(|candidate| hydra_nostr::received_reaction(&candidate).ok().flatten())
        .collect::<Vec<_>>();
    if let Some(reaction) = hydra_nostr::received_reaction(event)? {
        reactions.push(reaction);
    }
    reactions.retain(|reaction| {
        known_targets.contains(&reaction.target)
            && !known_events.contains(&reaction.source_event_id)
    });
    reactions.sort_by_key(|reaction| (reaction.occurred_at, reaction.source_event_id.clone()));
    reactions.dedup_by(|left, right| left.source_event_id == right.source_event_id);
    credit_remote_reaffirmations(store, &mut reactions);
    Ok(reactions)
}

fn projection_anchor_author(
    store: &DurableStore,
    incoming: &Event,
    heads: &[ObjectHead],
    projection: &PublicProjectionRecord,
) -> Result<Option<NostrPublicKey>, AppError> {
    if let Some(head) = heads
        .iter()
        .find(|head| head.anchor == projection.anchor)
        .or_else(|| store.state().heads.current_head(&projection.anchor).ok())
    {
        return Ok(Some(head.author.clone()));
    }
    let incoming_is_anchor = matches!(
        incoming.kind,
        Kind::TextNote
            | Kind::Thread
            | Kind::Comment
            | Kind::LongFormTextNote
            | Kind::Custom(20..=22)
    );
    if incoming_is_anchor && incoming.id.to_hex() == projection.anchor.as_str() {
        return Ok(Some(NostrPublicKey::parse(
            incoming
                .pubkey
                .to_bech32()
                .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?,
        )?));
    }
    let Some(stored) = store
        .state()
        .received_events
        .get(projection.anchor.as_str())
    else {
        return Ok(None);
    };
    let stored = Event::from_json(stored)
        .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?;
    if !matches!(
        stored.kind,
        Kind::TextNote
            | Kind::Thread
            | Kind::Comment
            | Kind::LongFormTextNote
            | Kind::Custom(20..=22)
    ) {
        return Ok(None);
    }
    Ok(Some(NostrPublicKey::parse(
        stored
            .pubkey
            .to_bech32()
            .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?,
    )?))
}

fn validate_public_event_shape(event: &Event) -> Result<(), AppError> {
    const MAX_TAGS: usize = 256;
    const MAX_TAG_FIELDS: usize = 16;
    const MAX_TAG_FIELD_BYTES: usize = 4_096;
    if event.tags.len() > MAX_TAGS
        || event.tags.iter().any(|tag| {
            let fields = tag.as_slice();
            fields.len() > MAX_TAG_FIELDS
                || fields.iter().any(|field| field.len() > MAX_TAG_FIELD_BYTES)
        })
    {
        return Err(DomainError::InvalidObjectShape.into());
    }
    Ok(())
}

fn credit_remote_reaffirmations(store: &DurableStore, incoming: &mut [ReactionRecord]) {
    for index in 0..incoming.len() {
        if !incoming[index].value.can_reaffirm() {
            continue;
        }
        let actor = &incoming[index].actor;
        let target = &incoming[index].target;
        let value = &incoming[index].value;
        let baseline = store
            .state()
            .reactions
            .iter()
            .chain(incoming[..index].iter())
            .rfind(|known| {
                known.actor == *actor
                    && known.target == *target
                    && known.value == *value
                    && known.credited_reaffirmation
            })
            .or_else(|| {
                store
                    .state()
                    .reactions
                    .iter()
                    .chain(incoming[..index].iter())
                    .find(|known| {
                        known.actor == *actor && known.target == *target && known.value == *value
                    })
            })
            .map(|known| known.occurred_at);
        incoming[index].credited_reaffirmation = baseline.is_some_and(|baseline| {
            incoming[index].occurred_at >= baseline.saturating_add(18 * 60 * 60)
        });
    }
}

pub trait PersonaEventSink {
    /// Appends the public persona event after the secret is safely stored.
    ///
    /// # Errors
    ///
    /// Returns an error when public durable state cannot be committed.
    fn append_persona(&mut self, persona: Persona, recorded_at: u64) -> Result<(), StoreError>;
}

impl PersonaEventSink for DurableStore {
    fn append_persona(&mut self, persona: Persona, recorded_at: u64) -> Result<(), StoreError> {
        self.append(DurableEvent::PersonaCreated(persona), recorded_at)?;
        Ok(())
    }
}

impl<S: SecretStore> PersonaService<S> {
    #[must_use]
    pub fn new(secrets: S) -> Self {
        Self { secrets }
    }

    /// Generates and transactionally persists one Nostr persona.
    ///
    /// # Errors
    ///
    /// Returns an error if key encoding, credential storage, or durable event
    /// append fails. A failed append triggers credential rollback.
    pub fn create(
        &self,
        store: &mut impl PersonaEventSink,
        display_name: String,
        recorded_at: u64,
    ) -> Result<Persona, AppError> {
        Persona::validate_display_name(&display_name)?;
        let keys = Keys::generate();
        let persona = Persona {
            id: PersonaId::new(),
            public_key: NostrPublicKey::parse(
                keys.public_key()
                    .to_bech32()
                    .map_err(|error| AppError::KeyEncoding(error.to_string()))?,
            )?,
            display_name,
            reddit_account: None,
        };
        let signing_secret = keys
            .secret_key()
            .to_bech32()
            .map_err(|error| AppError::KeyEncoding(error.to_string()))?;
        let credential = PersonaCredential::Local { signing_secret }.encode()?;
        self.secrets.set(persona.id, &credential)?;
        if let Err(error) = store.append_persona(persona.clone(), recorded_at) {
            self.secrets.delete(persona.id)?;
            return Err(error.into());
        }
        Ok(persona)
    }

    /// Imports an existing Nostr secret as a persona without exposing it to the
    /// durable event log.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid key material, invalid display metadata, a
    /// credential-store failure, or a failed durable append.
    pub fn import(
        &self,
        store: &mut impl PersonaEventSink,
        display_name: String,
        secret: &str,
        recorded_at: u64,
    ) -> Result<Persona, AppError> {
        Persona::validate_display_name(&display_name)?;
        let keys = Keys::parse(secret)
            .map_err(|error| AppError::Credential(format!("invalid Nostr secret: {error}")))?;
        let persona = Persona {
            id: PersonaId::new(),
            public_key: NostrPublicKey::parse(
                keys.public_key()
                    .to_bech32()
                    .map_err(|error| AppError::KeyEncoding(error.to_string()))?,
            )?,
            display_name,
            reddit_account: None,
        };
        let signing_secret = keys
            .secret_key()
            .to_bech32()
            .map_err(|error| AppError::KeyEncoding(error.to_string()))?;
        let credential = PersonaCredential::Local { signing_secret }.encode()?;
        self.secrets.set(persona.id, &credential)?;
        if let Err(error) = store.append_persona(persona.clone(), recorded_at) {
            self.secrets.delete(persona.id)?;
            return Err(error.into());
        }
        Ok(persona)
    }

    /// Connects a NIP-46 bunker as a Hydra persona while keeping only the
    /// transport client key and an unrelated local-storage key on this device.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed connection data, signer refusal or
    /// timeout, credential-vault failure, or failed durable persistence.
    pub async fn connect_remote(
        &self,
        store: &mut impl PersonaEventSink,
        display_name: String,
        bunker_uri: &str,
        recorded_at: u64,
    ) -> Result<Persona, AppError> {
        Persona::validate_display_name(&display_name)?;
        let uri = NostrConnectURI::parse(bunker_uri)
            .map_err(|error| AppError::Credential(format!("invalid NIP-46 URI: {error}")))?;
        let client_keys = Keys::generate();
        let remote = NostrConnect::new(uri, client_keys.clone(), Duration::from_secs(60), None)
            .map_err(|error| AppError::Credential(error.to_string()))?;
        let public_key = remote
            .get_public_key()
            .await
            .map_err(|error| AppError::Credential(error.to_string()))?;
        let persona = Persona {
            id: PersonaId::new(),
            public_key: NostrPublicKey::parse(
                public_key
                    .to_bech32()
                    .map_err(|error| AppError::KeyEncoding(error.to_string()))?,
            )?,
            display_name,
            reddit_account: None,
        };
        let credential = PersonaCredential::Remote {
            bunker_uri: bunker_uri.to_owned(),
            client_secret: client_keys
                .secret_key()
                .to_bech32()
                .map_err(|error| AppError::KeyEncoding(error.to_string()))?,
            user_public_key: public_key
                .to_bech32()
                .map_err(|error| AppError::KeyEncoding(error.to_string()))?,
            storage_secret: Keys::generate()
                .secret_key()
                .to_bech32()
                .map_err(|error| AppError::KeyEncoding(error.to_string()))?,
        }
        .encode()?;
        self.secrets.set(persona.id, &credential)?;
        if let Err(error) = store.append_persona(persona.clone(), recorded_at) {
            self.secrets.delete(persona.id)?;
            return Err(error.into());
        }
        Ok(persona)
    }

    /// Links or unlinks the persona's single Reddit identity without exposing
    /// Reddit credentials to public durable state.
    ///
    /// # Errors
    ///
    /// Returns an error when the persona is missing, the one-to-one identity
    /// invariant would be broken, or the update cannot be stored.
    pub fn set_reddit_account(
        &self,
        store: &mut DurableStore,
        persona_id: PersonaId,
        account: Option<hydra_domain::RedditAccountId>,
        recorded_at: u64,
    ) -> Result<Persona, AppError> {
        let mut persona = store
            .state()
            .personas
            .get(persona_id)
            .cloned()
            .ok_or(DomainError::MissingPersona)?;
        persona.reddit_account = account;
        store.append(DurableEvent::PersonaUpdated(persona.clone()), recorded_at)?;
        Ok(persona)
    }

    /// Publishes and durably records one persona's standard NIP-65 relay list.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing persona, invalid relay URLs, signing, or
    /// durable persistence failure.
    pub fn configure_relays(
        &self,
        store: &mut DurableStore,
        persona_id: PersonaId,
        read: Vec<String>,
        write: Vec<String>,
        changed_at: u64,
    ) -> Result<(), AppError> {
        if store
            .state()
            .persona_relays
            .get(&persona_id)
            .is_some_and(|known| known == &(read.clone(), write.clone()))
        {
            return Ok(());
        }
        let (_, keys) = persona_and_keys(&self.secrets, store, persona_id)?;
        let event = hydra_nostr::persona_relay_list(&keys, &read, &write, changed_at)?;
        let mut publication_relays = read.iter().chain(&write).cloned().collect::<Vec<_>>();
        publication_relays.sort();
        publication_relays.dedup();
        store.append(
            DurableEvent::PersonaRelaysChanged {
                persona: persona_id,
                read,
                write,
                outbound: outbound_event(&event, &publication_relays),
            },
            changed_at,
        )?;
        Ok(())
    }

    /// Publishes the persona's standard Nostr profile and updates its local
    /// display name through the same durable event.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing persona, invalid name, signing, or local
    /// persistence failure.
    pub fn publish_profile(
        &self,
        store: &mut DurableStore,
        persona_id: PersonaId,
        display_name: String,
        flair: Option<String>,
        relays: &[String],
        changed_at: u64,
    ) -> Result<(), AppError> {
        Persona::validate_display_name(&display_name)?;
        let (persona, keys) = persona_and_keys(&self.secrets, store, persona_id)?;
        let flair = flair.map(FlairText::parse).transpose()?;
        let existing_content = current_profile_content(store, &persona.public_key);
        let event = hydra_nostr::profile_metadata(
            &keys,
            &display_name,
            flair.as_ref(),
            existing_content.as_deref(),
            changed_at,
        )?;
        store.append(
            DurableEvent::PersonaProfilePublished {
                persona: persona_id,
                display_name,
                flair,
                outbound: outbound_event(&event, relays),
            },
            changed_at,
        )?;
        Ok(())
    }

    /// Publishes an already verified Reddit identity artifact as a NIP-39
    /// external-identity claim.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid proof data, missing keys, signing, or local
    /// persistence failure.
    pub fn publish_reddit_identity_proof(
        &self,
        store: &mut DurableStore,
        proof: hydra_domain::RedditIdentityProof,
        relays: &[String],
    ) -> Result<(), AppError> {
        let (_, keys) = persona_and_keys(&self.secrets, store, proof.persona)?;
        let event = hydra_nostr::reddit_identity_proof(&keys, &proof)?;
        let recorded_at = proof.published_at;
        store.append(
            DurableEvent::RedditIdentityProofPublished {
                proof,
                outbound: outbound_event(&event, relays),
            },
            recorded_at,
        )?;
        Ok(())
    }
}

/// Reports whether the persona delegates signing through NIP-46 without
/// revealing any credential material.
///
/// # Errors
///
/// Returns an error when the credential cannot be read or decoded.
pub fn persona_uses_remote_signer(
    secrets: &impl SecretStore,
    persona_id: PersonaId,
) -> Result<bool, AppError> {
    Ok(matches!(
        PersonaCredential::decode(&secrets.get(persona_id)?)?,
        PersonaCredential::Remote { .. }
    ))
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DraftService;

impl DraftService {
    /// Encrypts and stores the latest persona-owned draft state.
    ///
    /// # Errors
    ///
    /// Returns an error if the draft shape is invalid, its persona is missing,
    /// encryption fails, or durable storage rejects the event.
    pub fn save(
        secrets: &impl SecretStore,
        store: &mut DurableStore,
        draft: DraftRecord,
        recorded_at: u64,
    ) -> Result<(), AppError> {
        draft.validate()?;
        let (_, keys) = persona_and_keys(secrets, store, draft.persona)?;
        store_private_record(
            store,
            &keys,
            &PrivateRecord::Draft(draft),
            PrivateCommit {
                recorded_at,
                ..PrivateCommit::default()
            },
        )
    }
}

pub struct DiscussionService<S> {
    secrets: S,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SyncReport {
    pub accepted: usize,
    pub failed: usize,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SyncService;

impl SyncService {
    /// Publishes every event-relay pair that lacks accepted delivery evidence.
    ///
    /// # Errors
    ///
    /// Returns an error only when delivery evidence cannot be committed locally.
    /// Transport failures are recorded as retryable per-relay facts.
    pub async fn sync_pending(
        store: &mut DurableStore,
        publisher: &impl EventPublisher,
        recorded_at: u64,
    ) -> Result<SyncReport, AppError> {
        let pending = store
            .state()
            .outbound
            .values()
            .filter_map(|event| {
                let relays = event
                    .relays
                    .iter()
                    .filter(|relay| {
                        !matches!(
                            store
                                .state()
                                .deliveries
                                .get(&(event.event_id.clone(), (*relay).clone())),
                            Some(DeliveryState::Accepted)
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                (!relays.is_empty()).then(|| OutboundEvent {
                    event_id: event.event_id.clone(),
                    event_json: event.event_json.clone(),
                    relays,
                })
            })
            .collect::<Vec<_>>();
        let mut report = SyncReport::default();
        for event in pending {
            match publisher.publish(&event).await {
                Ok(delivery) => {
                    for relay in delivery.accepted {
                        if event.relays.contains(&relay) {
                            record_delivery(
                                store,
                                &event.event_id,
                                relay,
                                DeliveryState::Accepted,
                                recorded_at,
                            )?;
                            report.accepted += 1;
                        }
                    }
                    for (relay, reason) in delivery.rejected {
                        if event.relays.contains(&relay) {
                            record_delivery(
                                store,
                                &event.event_id,
                                relay,
                                DeliveryState::Rejected { reason },
                                recorded_at,
                            )?;
                            report.failed += 1;
                        }
                    }
                }
                Err(error) => {
                    for relay in &event.relays {
                        record_delivery(
                            store,
                            &event.event_id,
                            relay.clone(),
                            DeliveryState::Failed {
                                reason: error.to_string(),
                            },
                            recorded_at,
                        )?;
                        report.failed += 1;
                    }
                }
            }
        }
        Ok(report)
    }
}

fn record_delivery(
    store: &mut DurableStore,
    event_id: &str,
    relay: String,
    state: DeliveryState,
    recorded_at: u64,
) -> Result<(), StoreError> {
    store.append(
        DurableEvent::DeliveryRecorded {
            event_id: event_id.to_owned(),
            relay,
            state,
        },
        recorded_at,
    )?;
    Ok(())
}

pub struct CreatePost {
    pub persona_id: PersonaId,
    pub title: String,
    pub link_url: Option<String>,
    pub body: String,
    pub communities: Vec<CommunityKey>,
    pub relays: Vec<String>,
    pub recorded_at: u64,
}

pub struct SetPostFlair {
    pub persona_id: PersonaId,
    pub target: AnchorId,
    pub community: Option<CommunityKey>,
    pub flair: Option<String>,
    pub relays: Vec<String>,
    pub changed_at: u64,
}

pub struct ImportAuthoredPost {
    pub persona_id: PersonaId,
    pub title: String,
    pub body: String,
    pub communities: Vec<CommunityKey>,
    pub source: ExternalId,
    pub relays: Vec<String>,
    pub recorded_at: u64,
}

pub struct CreateNorm {
    pub persona_id: PersonaId,
    pub statement: String,
    pub community: CommunityKey,
    pub relays: Vec<String>,
    pub recorded_at: u64,
}

pub struct CreateComment {
    pub persona_id: PersonaId,
    pub parent_anchor: AnchorId,
    pub body: String,
    pub relays: Vec<String>,
    pub recorded_at: u64,
}

pub struct CreateExternalComment {
    pub persona_id: PersonaId,
    pub root: ExternalId,
    pub parent: ExternalId,
    pub source: Option<ExternalId>,
    pub communities: Vec<CommunityKey>,
    pub body: String,
    pub relays: Vec<String>,
    pub recorded_at: u64,
}

pub struct EditObject {
    pub persona_id: PersonaId,
    pub anchor: AnchorId,
    pub title: Option<String>,
    pub body: String,
    /// Replacement topic coordinates for posts. Comments and norms retain
    /// their inherited coordinates regardless of this value.
    pub communities: Option<Vec<CommunityKey>>,
    pub relays: Vec<String>,
    pub recorded_at: u64,
}

pub struct ReactToObject {
    pub persona_id: PersonaId,
    pub target: AnchorId,
    pub value: ReactionValue,
    pub relays: Vec<String>,
    pub recorded_at: u64,
}

pub struct CurateEvent {
    pub persona_id: PersonaId,
    pub source_event_json: String,
    pub communities: Vec<CommunityKey>,
    pub relays: Vec<String>,
    pub recorded_at: u64,
}

pub struct SetRevisit {
    pub persona_id: PersonaId,
    pub target: AnchorId,
    pub intent: RevisitIntent,
    pub due_at: Option<u64>,
    pub recorded_at: u64,
}

pub struct RemoveRevisit {
    pub persona_id: PersonaId,
    pub target: AnchorId,
    pub recorded_at: u64,
}

pub struct RequestObjectDisowning {
    pub persona_id: PersonaId,
    pub anchor: AnchorId,
    pub reason: String,
    pub relays: Vec<String>,
    pub requested_at: u64,
}

pub struct ArchiveService<S> {
    secrets: S,
}

pub struct PreserveAndPublishMedia {
    pub persona_id: PersonaId,
    pub source: std::path::PathBuf,
    pub object: AnchorId,
    pub mime_type: String,
    pub original_url: Option<String>,
    pub blob_servers: Vec<String>,
    pub relays: Vec<String>,
    pub max_bytes: u64,
    pub description: String,
    pub preserved_at: u64,
}

impl<S: SecretStore> ArchiveService<S> {
    #[must_use]
    pub fn new(secrets: S) -> Self {
        Self { secrets }
    }

    /// Stores an exact local capture receipt.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid provenance or durable append failure.
    pub fn record_manifest(
        store: &mut DurableStore,
        manifest: ArchiveManifest,
    ) -> Result<ArchiveManifest, AppError> {
        hydra_archive::validate_manifest(&manifest)?;
        if !store.state().personas.contains(manifest.observer) {
            return Err(DomainError::MissingPersona.into());
        }
        store.append(
            DurableEvent::ArchiveCaptured(manifest.clone()),
            manifest.captured_at,
        )?;
        Ok(manifest)
    }

    /// Preserves one user-selected file in the content-addressed local media
    /// store and links its manifest to an existing durable object.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing object, invalid metadata, oversized or
    /// unreadable input, or durable append failure.
    pub fn preserve_media(
        store: &mut DurableStore,
        media: &MediaStore,
        source: impl AsRef<std::path::Path>,
        object: AnchorId,
        mime_type: String,
        original_url: Option<String>,
        preserved_at: u64,
    ) -> Result<MediaManifest, AppError> {
        if !store.state().heads.contains(&object) {
            return Err(DomainError::MissingObject.into());
        }
        let manifest = media.preserve(source, object, mime_type, original_url, preserved_at)?;
        store.append(DurableEvent::MediaPreserved(manifest.clone()), preserved_at)?;
        Ok(manifest)
    }

    /// Preserves a public object's media locally, uploads exact bytes to each
    /// configured Blossom server, then queues standard NIP-94 metadata.
    ///
    /// Local preservation is committed before any network request. A failed
    /// server never erases that copy, and metadata is published only after at
    /// least one server returns a matching descriptor.
    ///
    /// # Errors
    ///
    /// Returns an error for an inapplicable persona/object, unsafe media,
    /// complete upload failure, malformed server response, or signing failure.
    pub fn preserve_and_publish_media(
        &self,
        store: &mut DurableStore,
        media: &MediaStore,
        request: PreserveAndPublishMedia,
    ) -> Result<MediaManifest, AppError> {
        let (persona, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let head = store
            .state()
            .heads
            .current_head(&request.object)
            .map_err(|_| DomainError::MissingObject)?
            .clone();
        if head.author != persona.public_key {
            return Err(AppError::NotObjectAuthor);
        }
        let mut manifest = media.preserve_with_limit(
            &request.source,
            request.object.clone(),
            request.mime_type.clone(),
            request.original_url.clone(),
            request.preserved_at,
            request.max_bytes,
        )?;
        store.append(
            DurableEvent::MediaPreservedFor {
                persona: request.persona_id,
                manifest: manifest.clone(),
            },
            request.preserved_at,
        )?;
        if request.blob_servers.is_empty() {
            return Ok(manifest);
        }
        upload_to_blossom(&keys, &request, &mut manifest)?;
        let event = hydra_nostr::file_metadata(
            &keys,
            &manifest,
            &request.description,
            request.preserved_at,
        )?;
        manifest.metadata_event_id = Some(event.id.to_hex());
        store.append(
            DurableEvent::MediaPublished {
                persona: request.persona_id,
                manifest: manifest.clone(),
                outbound: OutboundEvent {
                    event_id: event.id.to_hex(),
                    event_json: event.as_json(),
                    relays: request.relays.clone(),
                },
            },
            request.preserved_at,
        )?;
        let attachments = store
            .state()
            .media
            .values()
            .filter(|item| item.object == head.anchor && !item.blob_urls.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        let mut body = head.body.as_str().to_owned();
        for attachment in &attachments {
            if let Some(url) = attachment.blob_urls.first()
                && !body.contains(url)
            {
                body.push_str("\n\n");
                body.push_str(url);
            }
        }
        let edited_at = request.preserved_at.max(head.edited_at.saturating_add(1));
        let reference = EventReference {
            id: NostrEventId::from_hex(head.anchor.as_str())
                .map_err(|error| AppError::KeyEncoding(error.to_string()))?,
            kind: match head.kind {
                ObjectKind::Post | ObjectKind::Norm => Kind::Thread,
                ObjectKind::Comment => Kind::Comment,
            },
            author: PublicKey::parse(head.author.as_str())
                .map_err(|error| AppError::KeyEncoding(error.to_string()))?,
        };
        let head_event = hydra_nostr::object_head_with_media(
            &keys,
            reference,
            native_comment_scope(store, &head)?,
            head.title.as_deref(),
            &body,
            &head.communities,
            &attachments,
            edited_at,
        )?;
        let mut attached_head = head;
        attached_head.body = ContentBody::parse(body)?;
        attached_head.edited_at = edited_at;
        store.append(
            DurableEvent::NativeObjectChanged {
                head: attached_head,
                outbound: vec![OutboundEvent {
                    event_id: head_event.id.to_hex(),
                    event_json: head_event.as_json(),
                    relays: request.relays,
                }],
            },
            edited_at,
        )?;
        Ok(manifest)
    }
}

fn upload_to_blossom(
    signer: &impl EventSigner,
    request: &PreserveAndPublishMedia,
    manifest: &mut MediaManifest,
) -> Result<(), AppError> {
    let blossom = BlossomClient::new()?;
    let mut errors = Vec::new();
    for server in &request.blob_servers {
        let parsed =
            url::Url::parse(server).map_err(|error| AppError::Credential(error.to_string()))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| AppError::Credential("Blossom server has no host".to_owned()))?;
        let authorization = hydra_nostr::blossom_upload_authorization(
            signer,
            host,
            &manifest.sha256,
            request.preserved_at,
        )?;
        match blossom.upload(server, &request.source, manifest, &authorization) {
            Ok(descriptor) => manifest.blob_urls.push(descriptor.url),
            Err(error) => errors.push(format!("{server}: {error}")),
        }
    }
    manifest.blob_urls.sort();
    manifest.blob_urls.dedup();
    if manifest.blob_urls.is_empty() {
        return Err(AppError::Credential(format!(
            "local copy is safe; every Blossom upload failed: {}",
            errors.join("; ")
        )));
    }
    Ok(())
}

pub struct ProjectionService<S> {
    secrets: S,
}

pub struct SetFollow {
    pub persona_id: PersonaId,
    pub target: NostrPublicKey,
    pub public: bool,
    pub following: bool,
    pub relays: Vec<String>,
    pub changed_at: u64,
}

pub struct PublishFollowSet {
    pub persona_id: PersonaId,
    pub identifier: String,
    pub title: String,
    pub members: Vec<NostrPublicKey>,
    pub relays: Vec<String>,
    pub published_at: u64,
}

pub struct SetBlock {
    pub persona_id: PersonaId,
    pub target: NostrPublicKey,
    pub public: bool,
    pub blocked: bool,
    pub reason: Option<String>,
    pub relays: Vec<String>,
    pub changed_at: u64,
}

pub struct SetPersonJudgment {
    pub persona_id: PersonaId,
    pub target: NostrPublicKey,
    pub topic: Option<CommunityKey>,
    pub public: bool,
    pub faculty: flocking_core::Faculty,
    pub action: flocking_core::Action,
    pub reason: Option<String>,
    pub relays: Vec<String>,
    pub changed_at: u64,
}

pub struct SetContentJudgment {
    pub persona_id: PersonaId,
    pub target: AnchorId,
    pub topic: Option<CommunityKey>,
    pub public: bool,
    pub faculty: flocking_core::Faculty,
    pub action: flocking_core::Action,
    pub reason: Option<String>,
    pub relays: Vec<String>,
    pub changed_at: u64,
}

pub struct SetPersonSource {
    pub persona_id: PersonaId,
    pub source: NostrPublicKey,
    pub faculty: flocking_core::Faculty,
    pub global: bool,
    pub topics: BTreeSet<CommunityKey>,
    pub rank: Option<u32>,
    pub enabled: bool,
    pub completeness: flocking_core::Completeness,
    pub changed_at: u64,
}

pub struct SetReverseBlockSource {
    pub persona_id: PersonaId,
    pub source: NostrPublicKey,
    pub global: bool,
    pub topics: BTreeSet<CommunityKey>,
    pub enabled: bool,
    pub completeness: flocking_core::Completeness,
    pub changed_at: u64,
}

pub struct SetPinDismissal {
    pub persona_id: PersonaId,
    pub topic: CommunityKey,
    pub target: AnchorId,
    pub dismissed: bool,
    pub changed_at: u64,
}

pub struct RescuePerson {
    pub persona_id: PersonaId,
    pub target: NostrPublicKey,
    pub topic: Option<CommunityKey>,
    pub public: bool,
    pub relays: Vec<String>,
    pub changed_at: u64,
}

pub struct SetCommunityAppearance {
    pub persona_id: PersonaId,
    pub topic: CommunityKey,
    pub image: Option<flocking_core::CommunityImage>,
    pub public: bool,
    pub relays: Vec<String>,
    pub changed_at: u64,
}

pub struct SetCommunityColorScheme {
    pub persona_id: PersonaId,
    pub topic: CommunityKey,
    pub scheme: Option<CommunityColorScheme>,
    pub public: bool,
    pub relays: Vec<String>,
    pub changed_at: u64,
}

pub struct SetAppearanceSource {
    pub persona_id: PersonaId,
    pub source: NostrPublicKey,
    pub enabled: bool,
    pub complete: bool,
    pub changed_at: u64,
}

pub struct SetLocalFilter {
    pub persona_id: PersonaId,
    pub kind: LocalFilterKind,
    pub value: String,
    pub enabled: bool,
    pub changed_at: u64,
}

pub struct SetCommunitySubscription {
    pub persona_id: PersonaId,
    pub community: CommunityKey,
    pub public: bool,
    pub subscribed: bool,
    pub relays: Vec<String>,
    pub changed_at: u64,
}

pub struct SocialService<S> {
    secrets: S,
}

impl<S: SecretStore> SocialService<S> {
    #[must_use]
    pub fn new(secrets: S) -> Self {
        Self { secrets }
    }

    /// Updates one follow and republishes the standard NIP-02 list only when
    /// public state changed.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identities, signing, or local durability.
    pub fn set_follow(
        &self,
        store: &mut DurableStore,
        request: &SetFollow,
    ) -> Result<FollowRecord, AppError> {
        PublicKey::parse(request.target.as_str())
            .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?;
        let (_, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let prior_was_public = store
            .state()
            .follows
            .get(&(request.persona_id, request.target.clone()))
            .is_some_and(|follow| follow.public && follow.following);
        let follow = FollowRecord {
            persona: request.persona_id,
            target: request.target.clone(),
            public: request.public,
            following: request.following,
            changed_at: request.changed_at,
        };
        let mut outbound = Vec::new();
        if request.public || prior_was_public {
            let mut targets = store
                .state()
                .follows
                .values()
                .filter(|item| {
                    item.persona == request.persona_id
                        && item.target != request.target
                        && item.public
                        && item.following
                })
                .map(|item| item.target.clone())
                .collect::<Vec<_>>();
            if request.public && request.following {
                targets.push(request.target.clone());
            }
            let event = hydra_nostr::public_follow_list(&keys, &targets, request.changed_at)?;
            outbound.push(outbound_event(&event, &request.relays));
        }
        if request.public {
            store.append(
                DurableEvent::FollowChanged {
                    follow: follow.clone(),
                    outbound,
                },
                request.changed_at,
            )?;
        } else {
            let public_follow_retraction = prior_was_public.then(|| FollowRecord {
                persona: request.persona_id,
                target: request.target.clone(),
                public: true,
                following: false,
                changed_at: request.changed_at,
            });
            store_private_record(
                store,
                &keys,
                &PrivateRecord::Follow(follow.clone()),
                PrivateCommit {
                    public_follow_retraction,
                    outbound,
                    recorded_at: request.changed_at,
                    ..PrivateCommit::default()
                },
            )?;
        }
        if request.public || prior_was_public {
            self.set_person_judgment(
                store,
                &SetPersonJudgment {
                    persona_id: request.persona_id,
                    target: request.target.clone(),
                    topic: None,
                    public: true,
                    faculty: flocking_core::Faculty::Follow,
                    action: if request.public {
                        if request.following {
                            flocking_core::Action::Follow
                        } else {
                            flocking_core::Action::Unfollow
                        }
                    } else {
                        flocking_core::Action::Withdraw
                    },
                    reason: None,
                    relays: request.relays.clone(),
                    changed_at: request.changed_at,
                },
            )?;
        }
        Ok(follow)
    }

    /// Publishes a curated, explicitly selected persona relationship as a
    /// standard NIP-51 follow set. This never exposes local keyring membership.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid set metadata, identities, signing, or
    /// local durability.
    pub fn publish_follow_set(
        &self,
        store: &mut DurableStore,
        request: &PublishFollowSet,
    ) -> Result<PublicFollowSet, AppError> {
        let (_, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let set = PublicFollowSet {
            persona: request.persona_id,
            identifier: request.identifier.trim().to_owned(),
            title: request.title.trim().to_owned(),
            members: request.members.clone(),
            published_at: request.published_at,
        };
        set.validate()?;
        let event = hydra_nostr::public_follow_set(
            &keys,
            &set.identifier,
            &set.title,
            &set.members,
            set.published_at,
        )?;
        store.append(
            DurableEvent::PublicFollowSetPublished {
                set: set.clone(),
                outbound: outbound_event(&event, &request.relays),
            },
            set.published_at,
        )?;
        Ok(set)
    }

    /// Updates one ownerless topic subscription, using NIP-51 interests for
    /// public subscriptions and authenticated local encryption for private ones.
    ///
    /// # Errors
    ///
    /// Returns an error for missing persona keys, signing, or local durability.
    pub fn set_community_subscription(
        &self,
        store: &mut DurableStore,
        request: &SetCommunitySubscription,
    ) -> Result<CommunitySubscription, AppError> {
        let (_, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let prior_was_public = store
            .state()
            .subscriptions
            .get(&(request.persona_id, request.community.clone()))
            .is_some_and(|item| item.public && item.subscribed);
        let subscription = CommunitySubscription {
            persona: request.persona_id,
            community: request.community.clone(),
            public: request.public,
            subscribed: request.subscribed,
            changed_at: request.changed_at,
        };
        let mut outbound = Vec::new();
        if request.public || prior_was_public {
            let mut communities = store
                .state()
                .subscriptions
                .values()
                .filter(|item| {
                    item.persona == request.persona_id
                        && item.community != request.community
                        && item.public
                        && item.subscribed
                })
                .map(|item| item.community.clone())
                .collect::<Vec<_>>();
            if request.public && request.subscribed {
                communities.push(request.community.clone());
            }
            let event = hydra_nostr::public_interests(&keys, &communities, request.changed_at)?;
            outbound.push(outbound_event(&event, &request.relays));
        }
        if request.public {
            store.append(
                DurableEvent::CommunitySubscriptionChanged {
                    subscription: subscription.clone(),
                    outbound,
                },
                request.changed_at,
            )?;
        } else {
            let public_subscription_retraction = prior_was_public.then(|| CommunitySubscription {
                persona: request.persona_id,
                community: request.community.clone(),
                public: true,
                subscribed: false,
                changed_at: request.changed_at,
            });
            store_private_record(
                store,
                &keys,
                &PrivateRecord::CommunitySubscription(subscription.clone()),
                PrivateCommit {
                    public_subscription_retraction,
                    outbound,
                    recorded_at: request.changed_at,
                    ..PrivateCommit::default()
                },
            )?;
        }
        Ok(subscription)
    }

    /// Updates a local/public block and publishes honest NIP-51/NIP-32 events
    /// without claiming network-wide exclusion.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identities/reasons, signing, or durability.
    pub fn set_block(
        &self,
        store: &mut DurableStore,
        request: &SetBlock,
    ) -> Result<BlockRecord, AppError> {
        PublicKey::parse(request.target.as_str())
            .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?;
        let (_, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let prior_was_public = store
            .state()
            .blocks
            .get(&(request.persona_id, request.target.clone()))
            .is_some_and(|block| block.public && block.blocked);
        let block = BlockRecord {
            persona: request.persona_id,
            target: request.target.clone(),
            public: request.public,
            blocked: request.blocked,
            reason: request.reason.clone(),
            changed_at: request.changed_at,
        };
        block.validate()?;
        let mut outbound = Vec::new();
        if request.public || prior_was_public {
            let mut targets = store
                .state()
                .blocks
                .values()
                .filter(|item| {
                    item.persona == request.persona_id
                        && item.target != request.target
                        && item.public
                        && item.blocked
                })
                .map(|item| item.target.clone())
                .collect::<Vec<_>>();
            if request.public && request.blocked {
                targets.push(request.target.clone());
            }
            let event = hydra_nostr::public_mute_list(&keys, &targets, request.changed_at)?;
            outbound.push(outbound_event(&event, &request.relays));
            if request.blocked
                && let Some(reason) = &request.reason
            {
                let reason_event = hydra_nostr::public_block_reason(
                    &keys,
                    &request.target,
                    reason,
                    request.changed_at,
                )?;
                outbound.push(outbound_event(&reason_event, &request.relays));
            }
        }
        if request.public {
            store.append(
                DurableEvent::BlockChanged {
                    block: block.clone(),
                    outbound,
                },
                request.changed_at,
            )?;
        } else {
            let public_block_retraction = prior_was_public.then(|| BlockRecord {
                persona: request.persona_id,
                target: request.target.clone(),
                public: true,
                blocked: false,
                reason: None,
                changed_at: request.changed_at,
            });
            store_private_record(
                store,
                &keys,
                &PrivateRecord::Block(block.clone()),
                PrivateCommit {
                    public_block_retraction,
                    outbound,
                    recorded_at: request.changed_at,
                    ..PrivateCommit::default()
                },
            )?;
        }
        Ok(block)
    }

    /// Updates one direct block or silence judgment without mutating inherited
    /// state. Published global blocks also update the faithful NIP-51 mirror.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identity, scope, reason, signing, or
    /// encrypted/durable persistence.
    pub fn set_person_judgment(
        &self,
        store: &mut DurableStore,
        request: &SetPersonJudgment,
    ) -> Result<FlockingJudgmentRecord, AppError> {
        let (persona, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let author = hydra_nostr::flocking_public_key(&persona.public_key)?;
        let target = hydra_nostr::flocking_public_key(&request.target)?;
        let scope = request.topic.as_ref().map_or_else(
            || Ok(flocking_core::Scope::Global),
            |topic| {
                flocking_core::Topic::parse(topic.as_str())
                    .map(flocking_core::Scope::Topic)
                    .map_err(|error| AppError::Flocking(error.to_string()))
            },
        )?;
        if !matches!(
            request.faculty,
            flocking_core::Faculty::Block
                | flocking_core::Faculty::Silence
                | flocking_core::Faculty::Follow
        ) {
            return Err(AppError::Flocking(
                "person judgments support only follow, block, and silence".to_owned(),
            ));
        }
        let reason = request
            .reason
            .as_ref()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let target = flocking_core::Target::Person(target);
        let current = private_state(&self.secrets, store, request.persona_id)?;
        let since = if request.faculty == flocking_core::Faculty::Silence
            && request.action == flocking_core::Action::Silence
        {
            Some(
                continuous_silence_cutoff(store, &current, &author, &scope, &target)
                    .unwrap_or(request.changed_at),
            )
        } else {
            None
        };
        let local = flocking_core::Judgment {
            author,
            faculty: request.faculty,
            scope,
            target,
            action: request.action,
            created_at: request.changed_at,
            event_id: None,
            since,
            reason,
            evidence: flocking_core::JudgmentEvidence::Local,
        };
        local
            .validate()
            .map_err(|error| AppError::Flocking(error.to_string()))?;
        persist_direct_judgment(
            store,
            &keys,
            FlockingJudgmentRecord {
                persona: request.persona_id,
                public: request.public,
                judgment: local,
            },
            &request.relays,
            request.changed_at,
        )
    }

    /// Updates one direct hide or community-membership judgment for a stable object.
    /// The underlying object remains durable and independently inspectable.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing object, invalid scope/action, signing, or persistence.
    pub fn set_content_judgment(
        &self,
        store: &mut DurableStore,
        request: &SetContentJudgment,
    ) -> Result<FlockingJudgmentRecord, AppError> {
        if !store.state().heads.contains(&request.target) {
            return Err(DomainError::MissingObject.into());
        }
        if !matches!(
            request.faculty,
            flocking_core::Faculty::Hide
                | flocking_core::Faculty::CommunityMembership
                | flocking_core::Faculty::Pin
        ) {
            return Err(AppError::Flocking(
                "content judgments support only hide, community membership, and pin".to_owned(),
            ));
        }
        let (persona, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let author = hydra_nostr::flocking_public_key(&persona.public_key)?;
        let scope = request.topic.as_ref().map_or_else(
            || Ok(flocking_core::Scope::Global),
            |topic| {
                flocking_core::Topic::parse(topic.as_str())
                    .map(flocking_core::Scope::Topic)
                    .map_err(|error| AppError::Flocking(error.to_string()))
            },
        )?;
        let event_id = flocking_core::EventId::parse(request.target.as_str())
            .map_err(|error| AppError::Flocking(error.to_string()))?;
        let interaction_action = format!("{:?}", request.action).to_ascii_lowercase();
        let interaction_detail = Some(format!("{:?}", request.faculty).to_ascii_lowercase());
        let local = flocking_core::Judgment {
            author,
            faculty: request.faculty,
            scope,
            target: flocking_core::Target::Content(flocking_core::ContentTarget::Event(event_id)),
            action: request.action,
            created_at: request.changed_at,
            event_id: None,
            since: None,
            reason: request
                .reason
                .as_ref()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            evidence: flocking_core::JudgmentEvidence::Local,
        };
        local
            .validate()
            .map_err(|error| AppError::Flocking(error.to_string()))?;
        let record = persist_direct_judgment(
            store,
            &keys,
            FlockingJudgmentRecord {
                persona: request.persona_id,
                public: request.public,
                judgment: local,
            },
            &request.relays,
            request.changed_at,
        )?;
        store.record_content_interaction(
            &request.target,
            &request.persona_id.to_string(),
            &interaction_action,
            interaction_detail,
            request.changed_at,
        )?;
        Ok(record)
    }

    /// Enables or removes one ordinary judgment source in selected scopes.
    /// Configuration remains encrypted and persona-local.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid keys, ranks, scopes, or persistence.
    pub fn set_person_source(
        &self,
        store: &mut DurableStore,
        request: &SetPersonSource,
    ) -> Result<FlockingProfile, AppError> {
        if !is_ordinary_source_faculty(request.faculty) {
            return Err(AppError::Flocking(
                "ordinary sources support follow, block, silence, hide, community membership, and pin"
                    .to_owned(),
            ));
        }
        let (persona, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let persona_key = hydra_nostr::flocking_public_key(&persona.public_key)?;
        let source_key = hydra_nostr::flocking_public_key(&request.source)?;
        let current = private_state(&self.secrets, store, request.persona_id)?;
        let mut profile = current.flocking_profile.unwrap_or(FlockingProfile {
            persona: request.persona_id,
            config: flocking_core::Config {
                version: flocking_core::CONFIG_VERSION.to_owned(),
                persona: persona_key.clone(),
                sources: Vec::new(),
                appearance_sources: BTreeSet::new(),
                local_pin_dismissals: Vec::new(),
            },
            source_states: Vec::new(),
            appearance_complete_sources: BTreeSet::new(),
            changed_at: request.changed_at,
        });
        if profile.config.persona != persona_key {
            return Err(AppError::Flocking(
                "configuration belongs to a different persona".to_owned(),
            ));
        }
        profile.config.sources.retain_mut(|source| {
            if source.pubkey == source_key {
                source
                    .grants
                    .retain(|grant| grant.faculty != request.faculty);
            }
            !source.grants.is_empty() || source.reverse_blocks.is_some()
        });
        if request.enabled {
            let topics = request
                .topics
                .iter()
                .map(|topic| {
                    flocking_core::Topic::parse(topic.as_str())
                        .map_err(|error| AppError::Flocking(error.to_string()))
                })
                .collect::<Result<BTreeSet<_>, _>>()?;
            let grant = flocking_core::FacultyGrant {
                faculty: request.faculty,
                global: request.global,
                topics: topics.clone(),
                rank: request.rank,
            };
            let source = profile
                .config
                .sources
                .iter_mut()
                .find(|source| source.pubkey == source_key);
            if let Some(source) = source {
                source.grants.push(grant);
            } else {
                profile.config.sources.push(flocking_core::Source {
                    pubkey: source_key.clone(),
                    grants: vec![grant],
                    reverse_blocks: None,
                });
            }
        }
        replace_source_states(
            &mut profile,
            &source_key,
            request.faculty,
            request.completeness,
        );
        profile.changed_at = request.changed_at;
        profile.validate()?;
        store_private_record(
            store,
            &keys,
            &PrivateRecord::FlockingProfile(profile.clone()),
            PrivateCommit {
                recorded_at: request.changed_at,
                ..PrivateCommit::default()
            },
        )?;
        Ok(profile)
    }

    /// Selects one person's block judgments as a separate discovery source.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identities, scopes, configuration, or encrypted persistence.
    pub fn set_reverse_block_source(
        &self,
        store: &mut DurableStore,
        request: &SetReverseBlockSource,
    ) -> Result<FlockingProfile, AppError> {
        let (persona, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let persona_key = hydra_nostr::flocking_public_key(&persona.public_key)?;
        let source_key = hydra_nostr::flocking_public_key(&request.source)?;
        let current = private_state(&self.secrets, store, request.persona_id)?;
        let mut profile = current.flocking_profile.unwrap_or(FlockingProfile {
            persona: request.persona_id,
            config: flocking_core::Config {
                version: flocking_core::CONFIG_VERSION.to_owned(),
                persona: persona_key.clone(),
                sources: Vec::new(),
                appearance_sources: BTreeSet::new(),
                local_pin_dismissals: Vec::new(),
            },
            source_states: Vec::new(),
            appearance_complete_sources: BTreeSet::new(),
            changed_at: request.changed_at,
        });
        if profile.config.persona != persona_key {
            return Err(AppError::Flocking(
                "configuration belongs to a different persona".to_owned(),
            ));
        }
        let topics = request
            .topics
            .iter()
            .map(|topic| {
                flocking_core::Topic::parse(topic.as_str())
                    .map_err(|error| AppError::Flocking(error.to_string()))
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if let Some(source) = profile
            .config
            .sources
            .iter_mut()
            .find(|item| item.pubkey == source_key)
        {
            source.reverse_blocks = request.enabled.then(|| flocking_core::ReverseBlockGrant {
                global: request.global,
                topics: topics.clone(),
            });
        } else if request.enabled {
            profile.config.sources.push(flocking_core::Source {
                pubkey: source_key.clone(),
                grants: Vec::new(),
                reverse_blocks: Some(flocking_core::ReverseBlockGrant {
                    global: request.global,
                    topics: topics.clone(),
                }),
            });
        }
        profile
            .config
            .sources
            .retain(|source| !source.grants.is_empty() || source.reverse_blocks.is_some());
        replace_source_states(
            &mut profile,
            &source_key,
            flocking_core::Faculty::Block,
            request.completeness,
        );
        profile.changed_at = request.changed_at;
        profile.validate()?;
        store_private_record(
            store,
            &keys,
            &PrivateRecord::FlockingProfile(profile.clone()),
            PrivateCommit {
                recorded_at: request.changed_at,
                ..PrivateCommit::default()
            },
        )?;
        Ok(profile)
    }

    /// Dismisses or restores one inherited contextual pin locally.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing target, invalid topic, or encrypted persistence.
    pub fn set_pin_dismissal(
        &self,
        store: &mut DurableStore,
        request: &SetPinDismissal,
    ) -> Result<FlockingProfile, AppError> {
        if !store.state().heads.contains(&request.target) {
            return Err(DomainError::MissingObject.into());
        }
        let (persona, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let persona_key = hydra_nostr::flocking_public_key(&persona.public_key)?;
        let current = private_state(&self.secrets, store, request.persona_id)?;
        let mut profile = current.flocking_profile.unwrap_or(FlockingProfile {
            persona: request.persona_id,
            config: flocking_core::Config {
                version: flocking_core::CONFIG_VERSION.to_owned(),
                persona: persona_key.clone(),
                sources: Vec::new(),
                appearance_sources: BTreeSet::new(),
                local_pin_dismissals: Vec::new(),
            },
            source_states: Vec::new(),
            appearance_complete_sources: BTreeSet::new(),
            changed_at: request.changed_at,
        });
        if profile.config.persona != persona_key {
            return Err(AppError::Flocking(
                "configuration belongs to a different persona".to_owned(),
            ));
        }
        let dismissal = flocking_core::LocalPinDismissal {
            topic: flocking_core::Topic::parse(request.topic.as_str())
                .map_err(|error| AppError::Flocking(error.to_string()))?,
            target_type: flocking_core::PinTargetType::Event,
            target: request.target.as_str().to_owned(),
        };
        profile
            .config
            .local_pin_dismissals
            .retain(|item| item != &dismissal);
        if request.dismissed {
            profile.config.local_pin_dismissals.push(dismissal);
        }
        profile.changed_at = request.changed_at;
        profile.validate()?;
        store_private_record(
            store,
            &keys,
            &PrivateRecord::FlockingProfile(profile.clone()),
            PrivateCommit {
                recorded_at: request.changed_at,
                ..PrivateCommit::default()
            },
        )?;
        Ok(profile)
    }

    /// Follows and directly unblocks one person discovered through a selected source.
    ///
    /// # Errors
    ///
    /// Returns an error when either direct judgment cannot be signed or stored.
    pub fn rescue_person(
        &self,
        store: &mut DurableStore,
        request: &RescuePerson,
    ) -> Result<(), AppError> {
        self.set_follow(
            store,
            &SetFollow {
                persona_id: request.persona_id,
                target: request.target.clone(),
                public: request.public,
                following: true,
                relays: request.relays.clone(),
                changed_at: request.changed_at,
            },
        )?;
        self.set_person_judgment(
            store,
            &SetPersonJudgment {
                persona_id: request.persona_id,
                target: request.target.clone(),
                topic: request.topic.clone(),
                public: request.public,
                faculty: flocking_core::Faculty::Block,
                action: flocking_core::Action::Unblock,
                reason: None,
                relays: request.relays.clone(),
                changed_at: request.changed_at,
            },
        )?;
        Ok(())
    }

    /// Stores a direct community-image choice and optionally publishes its signed event.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid image metadata, missing credentials, signing, or persistence.
    pub fn set_community_appearance(
        &self,
        store: &mut DurableStore,
        request: &SetCommunityAppearance,
    ) -> Result<CommunityAppearanceRecord, AppError> {
        let (persona, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let appearance = flocking_core::CommunityAppearance {
            author: hydra_nostr::flocking_public_key(&persona.public_key)?,
            topic: flocking_core::Topic::parse(request.topic.as_str())
                .map_err(|error| AppError::Flocking(error.to_string()))?,
            image: request.image.clone(),
            created_at: request.changed_at,
            event_id: None,
        };
        appearance
            .validate()
            .map_err(|error| AppError::Flocking(error.to_owned()))?;
        let mut record = CommunityAppearanceRecord {
            persona: request.persona_id,
            public: request.public,
            appearance,
        };
        let mut outbound = Vec::new();
        if request.public {
            let event = hydra_nostr::community_appearance_event(&keys, &record.appearance)?;
            record.appearance = hydra_nostr::received_community_appearances(&event)?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    AppError::Flocking("signed appearance was not readable".to_owned())
                })?;
            outbound.push(outbound_event(&event, &request.relays));
        }
        store_private_record(
            store,
            &keys,
            &PrivateRecord::CommunityAppearance(record.clone()),
            PrivateCommit {
                outbound,
                recorded_at: request.changed_at,
                ..PrivateCommit::default()
            },
        )?;
        Ok(record)
    }

    /// Stores a direct community-color choice and optionally publishes its signed event.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid colors, missing credentials, signing, or persistence.
    pub fn set_community_color_scheme(
        &self,
        store: &mut DurableStore,
        request: &SetCommunityColorScheme,
    ) -> Result<CommunityColorChoiceRecord, AppError> {
        let (persona, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let choice = CommunityColorChoice {
            author: persona.public_key.clone(),
            topic: request.topic.clone(),
            scheme: request.scheme.clone(),
            created_at: request.changed_at,
            event_id: None,
        };
        choice.validate()?;
        let mut record = CommunityColorChoiceRecord {
            persona: request.persona_id,
            public: request.public,
            choice,
        };
        let mut outbound = Vec::new();
        if request.public {
            let event = hydra_nostr::community_color_choice_event(&keys, &record.choice)?;
            record.choice = hydra_nostr::received_community_color_choices(&event)?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    AppError::Protocol(hydra_nostr::ProtocolError::Nostr(
                        "signed community colors were not readable".to_owned(),
                    ))
                })?;
            outbound.push(outbound_event(&event, &request.relays));
        }
        store_private_record(
            store,
            &keys,
            &PrivateRecord::CommunityColorChoice(record.clone()),
            PrivateCommit {
                outbound,
                recorded_at: request.changed_at,
                ..PrivateCommit::default()
            },
        )?;
        Ok(record)
    }

    /// Selects whether one person's image choices influence this persona.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identities, self-selection, or encrypted persistence.
    pub fn set_appearance_source(
        &self,
        store: &mut DurableStore,
        request: &SetAppearanceSource,
    ) -> Result<FlockingProfile, AppError> {
        let (persona, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let persona_key = hydra_nostr::flocking_public_key(&persona.public_key)?;
        let source_key = hydra_nostr::flocking_public_key(&request.source)?;
        let current = private_state(&self.secrets, store, request.persona_id)?;
        let mut profile = current.flocking_profile.unwrap_or(FlockingProfile {
            persona: request.persona_id,
            config: flocking_core::Config {
                version: flocking_core::CONFIG_VERSION.to_owned(),
                persona: persona_key.clone(),
                sources: Vec::new(),
                appearance_sources: BTreeSet::new(),
                local_pin_dismissals: Vec::new(),
            },
            source_states: Vec::new(),
            appearance_complete_sources: BTreeSet::new(),
            changed_at: request.changed_at,
        });
        if profile.config.persona != persona_key {
            return Err(AppError::Flocking(
                "configuration belongs to a different persona".to_owned(),
            ));
        }
        profile.config.appearance_sources.remove(&source_key);
        profile.appearance_complete_sources.remove(&source_key);
        if request.enabled {
            profile.config.appearance_sources.insert(source_key.clone());
            if request.complete {
                profile.appearance_complete_sources.insert(source_key);
            }
        }
        profile.changed_at = request.changed_at;
        profile.validate()?;
        store_private_record(
            store,
            &keys,
            &PrivateRecord::FlockingProfile(profile.clone()),
            PrivateCommit {
                recorded_at: request.changed_at,
                ..PrivateCommit::default()
            },
        )?;
        Ok(profile)
    }

    /// Updates one encrypted, persona-local content filter.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid filter values, missing credentials, or
    /// failed authenticated local persistence.
    pub fn set_local_filter(
        &self,
        store: &mut DurableStore,
        request: &SetLocalFilter,
    ) -> Result<LocalFilterRecord, AppError> {
        let (_, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let filter = LocalFilterRecord {
            persona: request.persona_id,
            kind: request.kind,
            value: request.value.trim().to_owned(),
            enabled: request.enabled,
            changed_at: request.changed_at,
        };
        filter.validate()?;
        store_private_record(
            store,
            &keys,
            &PrivateRecord::LocalFilter(filter.clone()),
            PrivateCommit {
                recorded_at: request.changed_at,
                ..PrivateCommit::default()
            },
        )?;
        Ok(filter)
    }
}

pub struct MessagingService<S> {
    secrets: S,
}

pub struct SendDirectMessage {
    pub persona_id: PersonaId,
    pub recipient: NostrPublicKey,
    pub body: String,
    /// The recipient's advertised NIP-17 inbox relays.
    pub recipient_relays: Vec<String>,
    /// The sender's advertised NIP-17 inbox relays for the recoverable copy.
    pub sender_relays: Vec<String>,
    pub created_at: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MessageSyncReport {
    pub received: usize,
    pub rejected: usize,
}

impl<S: SecretStore> MessagingService<S> {
    #[must_use]
    pub fn new(secrets: S) -> Self {
        Self { secrets }
    }

    /// Publishes the persona's standard NIP-17 inbox relay declaration.
    ///
    /// # Errors
    ///
    /// Returns an error for missing persona keys, invalid relays, signing, or
    /// durable append failure.
    pub fn configure_inbox(
        &self,
        store: &mut DurableStore,
        persona_id: PersonaId,
        inbox_relays: Vec<String>,
        publish_relays: &[String],
        changed_at: u64,
    ) -> Result<(), AppError> {
        if store
            .state()
            .inbox_relays
            .get(&persona_id)
            .is_some_and(|known| *known == inbox_relays)
        {
            return Ok(());
        }
        let (_, keys) = persona_and_keys(&self.secrets, store, persona_id)?;
        let event = hydra_nostr::inbox_relays(&keys, &inbox_relays, changed_at)?;
        store.append(
            DurableEvent::InboxRelaysChanged {
                persona: persona_id,
                relays: inbox_relays,
                outbound: outbound_event(&event, publish_relays),
            },
            changed_at,
        )?;
        Ok(())
    }

    /// Creates recipient and sender NIP-17 gift wraps and commits them through
    /// the ordinary local-first outbox in one durable operation.
    ///
    /// # Errors
    ///
    /// Returns an error for missing inbox relays, invalid identities, wrapping
    /// failure, or local durability failure.
    pub async fn send(
        &self,
        store: &mut DurableStore,
        request: SendDirectMessage,
    ) -> Result<DirectMessageRecord, AppError> {
        if request.recipient_relays.is_empty() || request.sender_relays.is_empty() {
            return Err(DomainError::Empty.into());
        }
        ContentBody::parse(request.body.clone())?;
        let (_, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let wraps = hydra_nostr::private_message(
            &keys,
            &request.recipient,
            &request.body,
            request.created_at,
        )
        .await?;
        let message = DirectMessageRecord {
            persona: request.persona_id,
            peer: request.recipient,
            direction: MessageDirection::Sent,
            body: request.body,
            created_at: request.created_at,
            rumor_id: wraps.rumor_id,
            request: false,
        };
        let source_event_id = wraps.sender_copy.id.to_hex();
        let outbound = vec![
            outbound_event(&wraps.recipient, &request.recipient_relays),
            outbound_event(&wraps.sender_copy, &request.sender_relays),
        ];
        store_private_record(
            store,
            &keys,
            &PrivateRecord::DirectMessage(message.clone()),
            PrivateCommit {
                source_event_id: Some(source_event_id),
                outbound,
                recorded_at: request.created_at,
                ..PrivateCommit::default()
            },
        )?;
        Ok(message)
    }

    /// Authenticates an incoming gift wrap and stores its plaintext only inside
    /// the selected persona's encrypted private record stream.
    ///
    /// Unknown senders are marked as requests rather than entering the ordinary
    /// inbox. Re-ingesting the same rumor is idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed or wrongly addressed gift wrap, missing
    /// persona key, or durable append failure.
    pub async fn receive(
        &self,
        store: &mut DurableStore,
        persona_id: PersonaId,
        gift_wrap_json: &str,
        recorded_at: u64,
    ) -> Result<DirectMessageRecord, AppError> {
        if gift_wrap_json.len() > OutboundEvent::MAX_EVENT_BYTES {
            return Err(DomainError::TooLong {
                max: OutboundEvent::MAX_EVENT_BYTES,
            }
            .into());
        }
        let (persona, keys) = persona_and_keys(&self.secrets, store, persona_id)?;
        let gift_wrap = nostr::Event::from_json(gift_wrap_json)
            .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?;
        let unwrapped = hydra_nostr::unwrap_private_message(&keys, &gift_wrap).await?;
        let private = private_state(&self.secrets, store, persona_id)?;
        if let Some(existing) = private
            .messages
            .iter()
            .find(|message| message.rumor_id == unwrapped.rumor_id)
        {
            return Ok(existing.clone());
        }
        let outgoing = unwrapped.sender == persona.public_key;
        let peer = if outgoing {
            unwrapped
                .recipients
                .iter()
                .find(|recipient| **recipient != persona.public_key)
                .cloned()
                .ok_or_else(|| {
                    hydra_nostr::ProtocolError::Nostr(
                        "sender copy has no external recipient".to_owned(),
                    )
                })?
        } else {
            unwrapped.sender
        };
        let request =
            hydra_messaging::is_message_request(store, &private, persona_id, &peer, outgoing);
        let message = DirectMessageRecord {
            persona: persona_id,
            peer,
            direction: if outgoing {
                MessageDirection::Sent
            } else {
                MessageDirection::Received
            },
            body: unwrapped.body,
            created_at: unwrapped.created_at,
            rumor_id: unwrapped.rumor_id,
            request,
        };
        store_private_record(
            store,
            &keys,
            &PrivateRecord::DirectMessage(message.clone()),
            PrivateCommit {
                source_event_id: Some(gift_wrap.id.to_hex()),
                recorded_at,
                ..PrivateCommit::default()
            },
        )?;
        Ok(message)
    }

    /// Refreshes one persona's bounded NIP-17 inbox and ingests every valid
    /// gift wrap idempotently into encrypted local history.
    ///
    /// # Errors
    ///
    /// Returns an error for relay access, invalid wraps, key access, or local
    /// durability failure.
    pub async fn receive_from_relays(
        &self,
        store: &mut DurableStore,
        persona_id: PersonaId,
        relays: &[String],
        since: Option<u64>,
        recorded_at: u64,
    ) -> Result<MessageSyncReport, AppError> {
        let (_, keys) = persona_and_keys(&self.secrets, store, persona_id)?;
        let mut events = hydra_nostr::fetch_gift_wraps(relays, &keys, since).await?;
        events.sort_by_key(|event| event.created_at);
        let mut report = MessageSyncReport::default();
        for event in events {
            let before = private_state(&self.secrets, store, persona_id)?
                .messages
                .len();
            if let Err(error) = self
                .receive(store, persona_id, &event.as_json(), recorded_at)
                .await
            {
                if matches!(error, AppError::Protocol(_)) {
                    report.rejected += 1;
                    continue;
                }
                return Err(error);
            }
            let after = private_state(&self.secrets, store, persona_id)?
                .messages
                .len();
            report.received += usize::from(after > before);
        }
        Ok(report)
    }
}

impl<S: SecretStore> ProjectionService<S> {
    #[must_use]
    pub fn new(secrets: S) -> Self {
        Self { secrets }
    }

    /// Records and publishes one generic external-projection state transition.
    /// Adapter-specific side effects occur outside this service.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid persona keys, signing, state transition, or
    /// local durability failure.
    pub fn record(
        &self,
        store: &mut DurableStore,
        projection: Projection,
        relays: Vec<String>,
        recorded_at: u64,
    ) -> Result<Projection, AppError> {
        let (_, keys) = persona_and_keys(&self.secrets, store, projection.persona)?;
        let outbound = if projection.external_id.is_some()
            && hydra_nostr::public_projection_state(projection.state).is_some()
        {
            let event = hydra_nostr::projection_record(&keys, &projection, recorded_at)?;
            Some(OutboundEvent {
                event_id: event.id.to_hex(),
                event_json: event.as_json(),
                relays,
            })
        } else {
            None
        };
        store.append(
            DurableEvent::ProjectionChanged {
                projection: projection.clone(),
                outbound,
            },
            recorded_at,
        )?;
        Ok(projection)
    }
}

fn link_post_source(value: Option<String>) -> Result<Option<ExternalId>, AppError> {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let parsed = url::Url::parse(value.trim()).map_err(|_| DomainError::InvalidObjectShape)?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(DomainError::InvalidObjectShape.into());
    }
    Ok(Some(ExternalId::new("url", parsed.to_string())?))
}

impl<S: SecretStore> DiscussionService<S> {
    #[must_use]
    pub fn new(secrets: S) -> Self {
        Self { secrets }
    }

    /// Sets or withdraws this persona's one current flair choice for a post scope.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown post/persona, invalid flair, signer mismatch,
    /// or durable persistence failure.
    pub fn set_post_flair(
        &self,
        store: &mut DurableStore,
        request: SetPostFlair,
    ) -> Result<PostFlairChoice, AppError> {
        let (persona, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let head = store.state().heads.current_head(&request.target)?;
        if head.kind != ObjectKind::Post {
            return Err(DomainError::InvalidObjectShape.into());
        }
        if let Some(community) = &request.community
            && !head.communities.contains(community)
        {
            return Err(DomainError::InvalidObjectShape.into());
        }
        let mut choice = PostFlairChoice {
            author: persona.public_key,
            target: request.target,
            scope: request
                .community
                .map_or(PostFlairScope::All, PostFlairScope::Community),
            flair: request.flair.map(FlairText::parse).transpose()?,
            changed_at: request.changed_at,
            source_event_id: "pending".to_owned(),
        };
        let event = hydra_nostr::post_flair_choice_event(&keys, &choice)?;
        choice.source_event_id = event.id.to_hex();
        store.append(
            DurableEvent::PostFlairChoiceChanged {
                persona: request.persona_id,
                choice: choice.clone(),
                outbound: outbound_event(&event, &request.relays),
            },
            request.changed_at,
        )?;
        Ok(choice)
    }

    /// Publishes a standard NIP-09 request for the persona's immutable anchor
    /// and editable head. It records a disowning signal, not guaranteed erasure.
    ///
    /// # Errors
    ///
    /// Returns an error when the persona does not own the object, the reason is
    /// too long, signing fails, or local durability fails.
    pub fn request_disowning(
        &self,
        store: &mut DurableStore,
        request: RequestObjectDisowning,
    ) -> Result<(), AppError> {
        let (persona, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let head = store
            .state()
            .heads
            .current_head(&request.anchor)
            .map_err(AppError::Store)?;
        if head.author != persona.public_key {
            return Err(AppError::Domain(DomainError::IdentityConflict));
        }
        if request.reason.len() > 500 {
            return Err(AppError::Domain(DomainError::TooLong { max: 500 }));
        }
        let event = hydra_nostr::object_deletion_request(
            &keys,
            &request.anchor,
            &request.reason,
            request.requested_at,
        )?;
        store.append(
            DurableEvent::ObjectDisowningRequested {
                persona: request.persona_id,
                anchor: request.anchor,
                reason: request.reason,
                outbound: outbound_event(&event, &request.relays),
            },
            request.requested_at,
        )?;
        Ok(())
    }

    /// Creates a local-first NIP-7D post anchor and editable Hydra head as one
    /// durable transaction, queued for the configured relays.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown persona, invalid key/content/community,
    /// protocol construction failure, or durable append failure.
    pub fn create_post(
        &self,
        store: &mut DurableStore,
        request: CreatePost,
    ) -> Result<ObjectHead, AppError> {
        let CreatePost {
            persona_id,
            title,
            link_url,
            body,
            communities,
            relays,
            recorded_at,
        } = request;
        let (persona, keys) = persona_and_keys(&self.secrets, store, persona_id)?;
        ObjectHead::validate_title(&title)?;
        let content = ContentBody::parse_post(body)?;
        let link_source = link_post_source(link_url)?;
        if content.as_str().trim().is_empty() && link_source.is_none() {
            return Err(DomainError::Empty.into());
        }
        let anchor = if let Some(source) = &link_source {
            hydra_nostr::link_post_anchor(
                &keys,
                &title,
                content.as_str(),
                &communities,
                source,
                recorded_at,
            )?
        } else {
            hydra_nostr::post_anchor(&keys, &title, content.as_str(), &communities, recorded_at)?
        };
        let reference = EventReference {
            id: anchor.id,
            kind: anchor.kind,
            author: anchor.pubkey,
        };
        let head_event = hydra_nostr::object_head(
            &keys,
            reference,
            None,
            Some(&title),
            content.as_str(),
            &communities,
            recorded_at,
        )?;
        let head = ObjectHead {
            anchor: AnchorId::parse(anchor.id.to_hex())?,
            author: persona.public_key,
            kind: ObjectKind::Post,
            title: Some(title),
            body: content,
            communities,
            root: None,
            parent: None,
            external_root: None,
            external_parent: None,
            external_source: link_source,
            edited_at: recorded_at,
        };
        let outbound = vec![
            OutboundEvent {
                event_id: anchor.id.to_hex(),
                event_json: anchor.as_json(),
                relays: relays.clone(),
            },
            OutboundEvent {
                event_id: head_event.id.to_hex(),
                event_json: head_event.as_json(),
                relays,
            },
        ];
        store.append(
            DurableEvent::NativeObjectChanged {
                head: head.clone(),
                outbound,
            },
            recorded_at,
        )?;
        Ok(head)
    }

    /// Imports a post supplied by its author through an account-data export.
    ///
    /// The original Reddit permalink is retained with standard NIP-73/NIP-48
    /// tags. Empty relay lists create a signed local-only copy.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid content, source metadata, persona/signing
    /// state, Nostr construction, or durable append failure.
    pub fn import_authored_post(
        &self,
        store: &mut DurableStore,
        request: ImportAuthoredPost,
    ) -> Result<ObjectHead, AppError> {
        let (persona, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        ObjectHead::validate_title(&request.title)?;
        let content = ContentBody::parse_post(request.body)?;
        let anchor = hydra_nostr::imported_post_anchor(
            &keys,
            &request.title,
            content.as_str(),
            &request.communities,
            &request.source,
            request.recorded_at,
        )?;
        let reference = EventReference {
            id: anchor.id,
            kind: anchor.kind,
            author: anchor.pubkey,
        };
        let head_event = hydra_nostr::object_head(
            &keys,
            reference,
            None,
            Some(&request.title),
            content.as_str(),
            &request.communities,
            request.recorded_at,
        )?;
        let head = ObjectHead {
            anchor: AnchorId::parse(anchor.id.to_hex())?,
            author: persona.public_key,
            kind: ObjectKind::Post,
            title: Some(request.title),
            body: content,
            communities: request.communities,
            root: None,
            parent: None,
            external_root: Some(request.source.clone()),
            external_parent: None,
            external_source: Some(request.source),
            edited_at: request.recorded_at,
        };
        store.append(
            DurableEvent::NativeObjectChanged {
                head: head.clone(),
                outbound: vec![
                    outbound_event(&anchor, &request.relays),
                    outbound_event(&head_event, &request.relays),
                ],
            },
            request.recorded_at,
        )?;
        Ok(head)
    }

    /// Publishes a community norm as an ordinary discussable thread plus a
    /// standard NIP-32 classification; it grants no enforcement authority.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid persona/content, signing, or durability.
    pub fn create_norm(
        &self,
        store: &mut DurableStore,
        request: CreateNorm,
    ) -> Result<ObjectHead, AppError> {
        let (persona, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let body = ContentBody::parse(request.statement)?;
        let communities = vec![request.community];
        let anchor = hydra_nostr::post_anchor(
            &keys,
            "Community norm",
            body.as_str(),
            &communities,
            request.recorded_at,
        )?;
        let reference = EventReference {
            id: anchor.id,
            kind: anchor.kind,
            author: anchor.pubkey,
        };
        let head_event = hydra_nostr::object_head(
            &keys,
            reference,
            None,
            Some("Community norm"),
            body.as_str(),
            &communities,
            request.recorded_at,
        )?;
        let label = hydra_nostr::norm_label(&keys, reference, request.recorded_at)?;
        let head = ObjectHead {
            anchor: AnchorId::parse(anchor.id.to_hex())?,
            author: persona.public_key,
            kind: ObjectKind::Norm,
            title: Some("Community norm".to_owned()),
            body,
            communities,
            root: None,
            parent: None,
            external_root: None,
            external_parent: None,
            external_source: None,
            edited_at: request.recorded_at,
        };
        Self::commit_object(
            store,
            head,
            vec![anchor, head_event, label],
            &request.relays,
            request.recorded_at,
        )
    }

    /// Creates a NIP-22 nested comment and editable Hydra head locally before
    /// either signed event is offered to a relay.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing parent/root/persona, invalid content or
    /// key, protocol construction failure, or durable append failure.
    pub fn create_comment(
        &self,
        store: &mut DurableStore,
        request: CreateComment,
    ) -> Result<ObjectHead, AppError> {
        let CreateComment {
            persona_id,
            parent_anchor,
            body,
            relays,
            recorded_at,
        } = request;
        let (persona, keys) = persona_and_keys(&self.secrets, store, persona_id)?;
        let parent = store.state().heads.current_head(&parent_anchor)?.clone();
        if let Some(external_root) = parent.external_root.clone() {
            let communities = parent.communities.clone();
            let content = ContentBody::parse(body)?;
            let parent_reference = event_reference(&parent)?;
            let anchor = hydra_nostr::external_root_comment_anchor(
                &keys,
                content.as_str(),
                &external_root,
                parent_reference,
                &communities,
                recorded_at,
            )?;
            let anchor_reference = EventReference {
                id: anchor.id,
                kind: anchor.kind,
                author: anchor.pubkey,
            };
            let head_event = hydra_nostr::external_root_comment_head(
                &keys,
                anchor_reference,
                content.as_str(),
                &external_root,
                parent_reference,
                &communities,
                recorded_at,
            )?;
            let head = ObjectHead {
                anchor: AnchorId::parse(anchor.id.to_hex())?,
                author: persona.public_key,
                kind: ObjectKind::Comment,
                title: None,
                body: content,
                communities,
                root: None,
                parent: Some(parent_anchor),
                external_root: Some(external_root),
                external_parent: None,
                external_source: None,
                edited_at: recorded_at,
            };
            return Self::commit_object(
                store,
                head,
                vec![anchor, head_event],
                &relays,
                recorded_at,
            );
        }
        let root_anchor = parent.root.as_ref().unwrap_or(&parent.anchor).clone();
        let root = store.state().heads.current_head(&root_anchor)?.clone();
        let communities = root.communities.clone();
        let content = ContentBody::parse(body)?;
        let scope = CommentScope {
            root: event_reference(&root)?,
            parent: event_reference(&parent)?,
        };
        let anchor =
            hydra_nostr::comment_anchor(&keys, content.as_str(), scope, &communities, recorded_at)?;
        let reference = EventReference {
            id: anchor.id,
            kind: anchor.kind,
            author: anchor.pubkey,
        };
        let head_event = hydra_nostr::object_head(
            &keys,
            reference,
            Some(scope),
            None,
            content.as_str(),
            &communities,
            recorded_at,
        )?;
        let head = ObjectHead {
            anchor: AnchorId::parse(anchor.id.to_hex())?,
            author: persona.public_key,
            kind: ObjectKind::Comment,
            title: None,
            body: content,
            communities,
            root: Some(root_anchor),
            parent: Some(parent_anchor),
            external_root: None,
            external_parent: None,
            external_source: None,
            edited_at: recorded_at,
        };
        Self::commit_object(store, head, vec![anchor, head_event], &relays, recorded_at)
    }

    /// Creates an editable Hydra comment rooted in an external NIP-22 object.
    /// The external object remains externally authored and the new comment is
    /// valid even when no Reddit projection can be created.
    ///
    /// # Errors
    ///
    /// Returns an error for missing persona keys, content, communities,
    /// malformed external identifiers, signing, or local durability failure.
    pub fn create_external_comment(
        &self,
        store: &mut DurableStore,
        request: CreateExternalComment,
    ) -> Result<ObjectHead, AppError> {
        let (persona, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let content = ContentBody::parse(request.body)?;
        let scope = ExternalCommentScope {
            root: request.root.clone(),
            parent: request.parent.clone(),
        };
        let anchor = hydra_nostr::external_comment_anchor(
            &keys,
            content.as_str(),
            &scope,
            request.source.as_ref(),
            &request.communities,
            request.recorded_at,
        )?;
        let reference = EventReference {
            id: anchor.id,
            kind: anchor.kind,
            author: anchor.pubkey,
        };
        let head_event = hydra_nostr::external_comment_head(
            &keys,
            reference,
            content.as_str(),
            &scope,
            request.source.as_ref(),
            &request.communities,
            request.recorded_at,
        )?;
        let head = ObjectHead {
            anchor: AnchorId::parse(anchor.id.to_hex())?,
            author: persona.public_key,
            kind: ObjectKind::Comment,
            title: None,
            body: content,
            communities: request.communities,
            root: None,
            parent: None,
            external_root: Some(request.root),
            external_parent: Some(request.parent),
            external_source: request.source,
            edited_at: request.recorded_at,
        };
        Self::commit_object(
            store,
            head,
            vec![anchor, head_event],
            &request.relays,
            request.recorded_at,
        )
    }

    /// Replaces the editable head for an authored post or comment while keeping
    /// its immutable anchor and reply topology stable.
    ///
    /// # Errors
    ///
    /// Returns an error for missing content/persona, non-author edits, invalid
    /// keys, protocol construction failure, or durable append failure.
    pub fn edit_object(
        &self,
        store: &mut DurableStore,
        request: EditObject,
    ) -> Result<ObjectHead, AppError> {
        let EditObject {
            persona_id,
            anchor,
            title,
            body,
            communities,
            relays,
            recorded_at,
        } = request;
        let current = store.state().heads.current_head(&anchor)?.clone();
        let (persona, keys) = persona_and_keys(&self.secrets, store, persona_id)?;
        if current.author != persona.public_key {
            return Err(AppError::NotObjectAuthor);
        }
        let body = match current.kind {
            ObjectKind::Post => ContentBody::parse_post(body)?,
            ObjectKind::Comment | ObjectKind::Norm => ContentBody::parse(body)?,
        };
        let title = match current.kind {
            ObjectKind::Post => {
                Some(title.unwrap_or_else(|| current.title.clone().unwrap_or_default()))
            }
            ObjectKind::Comment => None,
            ObjectKind::Norm => current.title.clone(),
        };
        let communities = match current.kind {
            ObjectKind::Post => {
                let communities = communities.unwrap_or_else(|| current.communities.clone());
                if communities.is_empty() {
                    return Err(AppError::Protocol(
                        hydra_nostr::ProtocolError::MissingCommunity,
                    ));
                }
                communities
            }
            ObjectKind::Comment | ObjectKind::Norm => current.communities.clone(),
        };
        let edited_at = recorded_at.max(current.edited_at.saturating_add(1));
        let reference = event_reference(&current)?;
        let event = match (&current.external_root, &current.external_parent) {
            (Some(root), Some(parent)) => hydra_nostr::external_comment_head(
                &keys,
                reference,
                body.as_str(),
                &ExternalCommentScope {
                    root: root.clone(),
                    parent: parent.clone(),
                },
                current.external_source.as_ref(),
                &communities,
                edited_at,
            )?,
            (Some(root), None) => {
                let parent_reference = current
                    .parent
                    .as_ref()
                    .ok_or(DomainError::InvalidObjectShape)?;
                let parent = store.state().heads.current_head(parent_reference)?;
                hydra_nostr::external_root_comment_head(
                    &keys,
                    reference,
                    body.as_str(),
                    root,
                    event_reference(parent)?,
                    &communities,
                    edited_at,
                )?
            }
            _ => hydra_nostr::object_head(
                &keys,
                reference,
                native_comment_scope(store, &current)?,
                title.as_deref(),
                body.as_str(),
                &communities,
                edited_at,
            )?,
        };
        let mut head = current.revised(body, edited_at);
        head.title = title;
        head.communities = communities;
        Self::commit_object(store, head, vec![event], &relays, recorded_at)
    }

    /// Publishes one NIP-25 reaction and records Hydra's temporal interpretation.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid target/persona/reaction, signing failure, or
    /// durable append failure.
    pub fn react(
        &self,
        store: &mut DurableStore,
        request: ReactToObject,
    ) -> Result<ReactionRecord, AppError> {
        let ReactToObject {
            persona_id,
            target,
            value,
            relays,
            recorded_at,
        } = request;
        let (persona, keys) = persona_and_keys(&self.secrets, store, persona_id)?;
        let head = store.state().heads.current_head(&target)?.clone();
        let event = hydra_nostr::reaction(
            &keys,
            event_reference(&head)?,
            value.wire_value()?,
            recorded_at,
        )?;
        let credited_reaffirmation = value.can_reaffirm()
            && reaffirmation_baseline(store, &persona.public_key, &target, &value)
                .is_some_and(|baseline| recorded_at >= baseline.saturating_add(18 * 60 * 60));
        let reaction = ReactionRecord {
            actor: persona.public_key,
            target,
            value,
            occurred_at: recorded_at,
            credited_reaffirmation,
            source_event_id: event.id.to_hex(),
        };
        store.append(
            DurableEvent::ReactionRecorded {
                reaction: reaction.clone(),
                outbound: OutboundEvent {
                    event_id: event.id.to_hex(),
                    event_json: event.as_json(),
                    relays,
                },
            },
            recorded_at,
        )?;
        Ok(reaction)
    }

    /// Publishes a standard NIP-18 repost that categorizes an existing Nostr
    /// event into one or more ownerless Hydra topics.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid source event, persona, topic, signature,
    /// or durable append failure.
    pub fn curate(
        &self,
        store: &mut DurableStore,
        request: &CurateEvent,
    ) -> Result<OutboundEvent, AppError> {
        let (_, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        let source = Event::from_json(&request.source_event_json)
            .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?;
        let event = hydra_nostr::curation_repost(
            &keys,
            &source,
            &request.communities,
            request.recorded_at,
        )?;
        let outbound = outbound_event(&event, &request.relays);
        store.append(
            DurableEvent::PublicEventQueued {
                persona: request.persona_id,
                outbound: outbound.clone(),
            },
            request.recorded_at,
        )?;
        Ok(outbound)
    }

    /// Stores a private-by-default reminder without publishing a social event.
    ///
    /// # Errors
    ///
    /// Returns an error for missing persona/target, invalid intent, or durable
    /// append failure.
    pub fn set_revisit(
        &self,
        store: &mut DurableStore,
        request: SetRevisit,
    ) -> Result<RevisitRecord, AppError> {
        let (_, keys) = persona_and_keys(&self.secrets, store, request.persona_id)?;
        if !store.state().heads.contains(&request.target) {
            return Err(DomainError::MissingObject.into());
        }
        let interaction_detail = Some(format!("{:?}", request.intent).to_ascii_lowercase());
        let revisit = RevisitRecord {
            persona: request.persona_id,
            target: request.target,
            intent: request.intent,
            due_at: request.due_at,
            active: true,
        };
        revisit.validate()?;
        store_private_record(
            store,
            &keys,
            &PrivateRecord::Revisit(revisit.clone()),
            PrivateCommit {
                recorded_at: request.recorded_at,
                ..PrivateCommit::default()
            },
        )?;
        store.record_content_interaction(
            &revisit.target,
            &revisit.persona.to_string(),
            "saved",
            interaction_detail,
            request.recorded_at,
        )?;
        Ok(revisit)
    }

    /// Removes one private Revisit item through an encrypted tombstone while
    /// preserving the earlier memory events for replay and audit.
    ///
    /// # Errors
    ///
    /// Returns an error when the persona, target, or current Revisit entry is
    /// missing, or when the encrypted tombstone cannot be committed.
    pub fn remove_revisit(
        &self,
        store: &mut DurableStore,
        request: RemoveRevisit,
    ) -> Result<RevisitRecord, AppError> {
        let RemoveRevisit {
            persona_id,
            target,
            recorded_at,
        } = request;
        let (_, keys) = persona_and_keys(&self.secrets, store, persona_id)?;
        let mut revisit = private_state(&self.secrets, store, persona_id)?
            .revisits
            .get(&target)
            .cloned()
            .ok_or(DomainError::MissingObject)?;
        revisit.active = false;
        store_private_record(
            store,
            &keys,
            &PrivateRecord::Revisit(revisit.clone()),
            PrivateCommit {
                recorded_at,
                ..PrivateCommit::default()
            },
        )?;
        store.record_content_interaction(
            &revisit.target,
            &revisit.persona.to_string(),
            "unsaved",
            None,
            recorded_at,
        )?;
        Ok(revisit)
    }

    fn commit_object(
        store: &mut DurableStore,
        head: ObjectHead,
        events: Vec<nostr::Event>,
        relays: &[String],
        recorded_at: u64,
    ) -> Result<ObjectHead, AppError> {
        let outbound = events
            .into_iter()
            .map(|event| OutboundEvent {
                event_id: event.id.to_hex(),
                event_json: event.as_json(),
                relays: relays.to_vec(),
            })
            .collect();
        store.append(
            DurableEvent::NativeObjectChanged {
                head: head.clone(),
                outbound,
            },
            recorded_at,
        )?;
        Ok(head)
    }
}

fn outbound_event(event: &nostr::Event, relays: &[String]) -> OutboundEvent {
    OutboundEvent {
        event_id: event.id.to_hex(),
        event_json: event.as_json(),
        relays: relays.to_vec(),
    }
}

fn current_profile_content(store: &DurableStore, author: &NostrPublicKey) -> Option<String> {
    store
        .state()
        .received_events
        .values()
        .chain(store.state().outbound.values().map(|item| &item.event_json))
        .filter_map(|json| Event::from_json(json).ok())
        .filter(|event| {
            event.kind == Kind::Metadata
                && event
                    .pubkey
                    .to_bech32()
                    .is_ok_and(|key| key == author.as_str())
        })
        .max_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| right.id.cmp(&left.id))
        })
        .map(|event| event.content)
}

fn persist_direct_judgment(
    store: &mut DurableStore,
    keys: &PersonaSigningContext,
    direct: FlockingJudgmentRecord,
    relays: &[String],
    changed_at: u64,
) -> Result<FlockingJudgmentRecord, AppError> {
    if !direct.public {
        store_private_record(
            store,
            keys,
            &PrivateRecord::FlockingJudgment(direct.clone()),
            PrivateCommit {
                recorded_at: changed_at,
                ..PrivateCommit::default()
            },
        )?;
        return Ok(direct);
    }
    let event = hydra_nostr::flocking_judgment_event(keys, &direct.judgment)?;
    let published = hydra_nostr::received_flocking_judgments(&event)?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::Flocking("signed judgment was not readable".to_owned()))?;
    let mut outbound = vec![outbound_event(&event, relays)];
    if published.faculty == flocking_core::Faculty::Block
        && published.scope == flocking_core::Scope::Global
    {
        let mirror = block_mirror_with_legacy(store, direct.persona, &published)?;
        let event = hydra_nostr::public_mute_list(keys, &mirror, changed_at)?;
        outbound.push(outbound_event(&event, relays));
    }
    let record = FlockingJudgmentRecord {
        persona: direct.persona,
        public: true,
        judgment: published,
    };
    store.append(
        DurableEvent::FlockingJudgmentChanged {
            record: record.clone(),
            outbound,
        },
        changed_at,
    )?;
    Ok(record)
}

fn continuous_silence_cutoff(
    store: &DurableStore,
    private: &PrivateState,
    author: &flocking_core::PublicKey,
    scope: &flocking_core::Scope,
    target: &flocking_core::Target,
) -> Option<u64> {
    let mut judgments = store
        .state()
        .flocking_judgments
        .iter()
        .filter(|judgment| judgment.author == *author)
        .cloned()
        .collect::<Vec<_>>();
    judgments.extend(
        private
            .flocking_judgments
            .values()
            .map(|record| record.judgment.clone()),
    );
    flocking_core::canonical_current(&judgments)
        .into_iter()
        .find(|judgment| {
            judgment.author == *author
                && judgment.faculty == flocking_core::Faculty::Silence
                && judgment.scope == *scope
                && judgment.target == *target
                && judgment.action == flocking_core::Action::Silence
        })
        .and_then(|judgment| judgment.since)
}

fn is_ordinary_source_faculty(faculty: flocking_core::Faculty) -> bool {
    matches!(
        faculty,
        flocking_core::Faculty::Follow
            | flocking_core::Faculty::Block
            | flocking_core::Faculty::Silence
            | flocking_core::Faculty::Hide
            | flocking_core::Faculty::CommunityMembership
            | flocking_core::Faculty::Pin
    )
}

fn replace_source_states(
    profile: &mut FlockingProfile,
    source_key: &flocking_core::PublicKey,
    faculty: flocking_core::Faculty,
    completeness: flocking_core::Completeness,
) {
    profile
        .source_states
        .retain(|state| state.source != *source_key || state.faculty != faculty);
    let Some(source) = profile.config.source(source_key) else {
        return;
    };
    let mut scopes = BTreeSet::new();
    if let Some(grant) = source.grants.iter().find(|grant| grant.faculty == faculty) {
        if grant.global {
            scopes.insert(flocking_core::Scope::Global);
        }
        scopes.extend(
            grant
                .topics
                .iter()
                .cloned()
                .map(flocking_core::Scope::Topic),
        );
    }
    if faculty == flocking_core::Faculty::Block
        && let Some(grant) = &source.reverse_blocks
    {
        if grant.global {
            scopes.insert(flocking_core::Scope::Global);
        }
        scopes.extend(
            grant
                .topics
                .iter()
                .cloned()
                .map(flocking_core::Scope::Topic),
        );
    }
    profile
        .source_states
        .extend(scopes.into_iter().map(|scope| flocking_core::SourceState {
            source: source_key.clone(),
            faculty,
            scope,
            completeness,
        }));
}

fn block_mirror_with_legacy(
    store: &DurableStore,
    persona: PersonaId,
    pending: &flocking_core::Judgment,
) -> Result<Vec<NostrPublicKey>, AppError> {
    let mut judgments = store
        .state()
        .flocking_judgments
        .iter()
        .filter(|judgment| judgment.author == pending.author)
        .cloned()
        .collect::<Vec<_>>();
    judgments.push(pending.clone());
    let current = flocking_core::canonical_current(&judgments);
    let mut mirror = flocking_nostr::block_mirror(&judgments, &pending.author);
    for legacy in store
        .state()
        .blocks
        .values()
        .filter(|record| record.persona == persona && record.public && record.blocked)
    {
        let key = hydra_nostr::flocking_public_key(&legacy.target)?;
        let has_canonical = current.iter().any(|judgment| {
            judgment.faculty == flocking_core::Faculty::Block
                && judgment.scope == flocking_core::Scope::Global
                && judgment.target == flocking_core::Target::Person(key.clone())
        });
        if !has_canonical {
            mirror.insert(key);
        }
    }
    Ok(mirror
        .into_iter()
        .map(|key| NostrPublicKey::parse(key.to_string()))
        .collect::<Result<Vec<_>, _>>()?)
}

#[derive(Debug, Default)]
struct PrivateCommit {
    source_event_id: Option<String>,
    public_follow_retraction: Option<FollowRecord>,
    public_block_retraction: Option<BlockRecord>,
    public_subscription_retraction: Option<CommunitySubscription>,
    outbound: Vec<OutboundEvent>,
    recorded_at: u64,
}

fn store_private_record(
    store: &mut DurableStore,
    keys: &PersonaSigningContext,
    private: &PrivateRecord,
    commit: PrivateCommit,
) -> Result<(), AppError> {
    private.validate()?;
    let plaintext = serde_json::to_string(&private)?;
    let record = EncryptedPrivateRecord {
        persona: match &private {
            PrivateRecord::Draft(item) => item.persona,
            PrivateRecord::Revisit(item) => item.persona,
            PrivateRecord::Follow(item) => item.persona,
            PrivateRecord::Block(item) => item.persona,
            PrivateRecord::DirectMessage(item) => item.persona,
            PrivateRecord::CommunitySubscription(item) => item.persona,
            PrivateRecord::LocalFilter(item) => item.persona,
            PrivateRecord::FlockingProfile(item) => item.persona,
            PrivateRecord::FlockingJudgment(item) => item.persona,
            PrivateRecord::CommunityAppearance(item) => item.persona,
            PrivateRecord::CommunityColorChoice(item) => item.persona,
        },
        ciphertext: hydra_nostr::encrypt_private(&keys.storage_keys, &plaintext)?,
        stored_at: commit.recorded_at,
        source_event_id: commit.source_event_id,
    };
    store.append(
        DurableEvent::PrivateRecordStored {
            record,
            public_follow_retraction: commit.public_follow_retraction,
            public_block_retraction: commit.public_block_retraction,
            public_subscription_retraction: commit.public_subscription_retraction,
            outbound: commit.outbound,
        },
        commit.recorded_at,
    )?;
    Ok(())
}

/// Decrypts the selected persona's private local records without exposing other
/// personas from the same keyring.
///
/// # Errors
///
/// Returns an error for a missing persona key, unauthentic ciphertext, or an
/// incompatible private-record payload.
pub fn private_records(
    secrets: &impl SecretStore,
    store: &DurableStore,
    persona_id: PersonaId,
) -> Result<Vec<PrivateRecord>, AppError> {
    let (_, keys) = persona_and_keys(secrets, store, persona_id)?;
    store
        .state()
        .private_records
        .iter()
        .filter(|record| record.persona == persona_id)
        .map(|record| {
            let plaintext = hydra_nostr::decrypt_private(&keys.storage_keys, &record.ciphertext)?;
            let private: PrivateRecord = serde_json::from_str(&plaintext)?;
            private.validate()?;
            Ok(private)
        })
        .collect()
}

/// Replays the selected persona's current private social and memory state.
///
/// # Errors
///
/// Returns an error when any selected encrypted record cannot be authenticated
/// and decoded. Hydra does not silently discard corrupt private history.
pub fn private_state(
    secrets: &impl SecretStore,
    store: &DurableStore,
    persona_id: PersonaId,
) -> Result<PrivateState, AppError> {
    let mut state = PrivateState::default();
    for record in private_records(secrets, store, persona_id)? {
        match record {
            PrivateRecord::Draft(item) => {
                if item.discarded {
                    state.drafts.remove(&item.id);
                } else {
                    state.drafts.insert(item.id.clone(), item);
                }
            }
            PrivateRecord::Revisit(item) => {
                if item.active {
                    state.revisits.insert(item.target.clone(), item);
                } else {
                    state.revisits.remove(&item.target);
                }
            }
            PrivateRecord::Follow(item) => {
                state.follows.insert(item.target.clone(), item);
            }
            PrivateRecord::Block(item) => {
                state.blocks.insert(item.target.clone(), item);
            }
            PrivateRecord::DirectMessage(item) => state.messages.push(item),
            PrivateRecord::CommunitySubscription(item) => {
                state.subscriptions.insert(item.community.clone(), item);
            }
            PrivateRecord::LocalFilter(item) => {
                let key = (item.kind, item.value.clone());
                if item.enabled {
                    state.filters.insert(key, item);
                } else {
                    state.filters.remove(&key);
                }
            }
            PrivateRecord::FlockingProfile(item) => {
                state.flocking_profile = Some(item);
            }
            PrivateRecord::FlockingJudgment(item) => {
                state
                    .flocking_judgments
                    .insert(item.judgment.address(), item);
            }
            PrivateRecord::CommunityAppearance(item) => {
                state
                    .community_appearances
                    .insert(item.appearance.topic.to_string(), item);
            }
            PrivateRecord::CommunityColorChoice(item) => {
                state
                    .community_color_choices
                    .insert(item.choice.topic.as_str().to_owned(), item);
            }
        }
    }
    Ok(state)
}

fn persona_and_keys(
    secrets: &impl SecretStore,
    store: &DurableStore,
    persona_id: PersonaId,
) -> Result<(Persona, PersonaSigningContext), AppError> {
    let persona = store
        .state()
        .personas
        .get(persona_id)
        .cloned()
        .ok_or(DomainError::MissingPersona)?;
    let credential = PersonaCredential::decode(&secrets.get(persona_id)?)?;
    let context = match credential {
        PersonaCredential::Local { signing_secret } => {
            let keys = Keys::parse(&signing_secret)
                .map_err(|error| AppError::KeyEncoding(error.to_string()))?;
            PersonaSigningContext {
                signer: HydraSigner::local(keys.clone()),
                storage_keys: keys,
            }
        }
        PersonaCredential::Remote {
            bunker_uri,
            client_secret,
            user_public_key,
            storage_secret,
        } => {
            let uri = NostrConnectURI::parse(&bunker_uri)
                .map_err(|error| AppError::Credential(error.to_string()))?;
            let client_keys = Keys::parse(&client_secret)
                .map_err(|error| AppError::Credential(error.to_string()))?;
            let public_key = PublicKey::parse(&user_public_key)
                .map_err(|error| AppError::Credential(error.to_string()))?;
            let remote = NostrConnect::new(uri, client_keys, Duration::from_secs(60), None)
                .map_err(|error| AppError::Credential(error.to_string()))?;
            remote
                .non_secure_set_user_public_key(public_key)
                .map_err(|error| AppError::Credential(error.to_string()))?;
            PersonaSigningContext {
                signer: HydraSigner::remote(public_key, Arc::new(remote)),
                storage_keys: Keys::parse(&storage_secret)
                    .map_err(|error| AppError::Credential(error.to_string()))?,
            }
        }
    };
    let encoded = context
        .public_key()
        .to_bech32()
        .map_err(|error| AppError::KeyEncoding(error.to_string()))?;
    if encoded != persona.public_key.to_string() {
        return Err(AppError::PersonaKeyMismatch);
    }
    Ok((persona, context))
}

fn reaffirmation_baseline(
    store: &DurableStore,
    actor: &NostrPublicKey,
    target: &AnchorId,
    value: &ReactionValue,
) -> Option<u64> {
    let matching = store
        .state()
        .reactions
        .iter()
        .filter(|reaction| {
            reaction.actor == *actor && reaction.target == *target && reaction.value == *value
        })
        .collect::<Vec<_>>();
    matching
        .iter()
        .rev()
        .find(|reaction| reaction.credited_reaffirmation)
        .map(|reaction| reaction.occurred_at)
        .or_else(|| matching.first().map(|reaction| reaction.occurred_at))
}

fn event_reference(head: &ObjectHead) -> Result<EventReference, AppError> {
    Ok(EventReference {
        id: NostrEventId::parse(head.anchor.as_str())
            .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?,
        kind: match head.kind {
            ObjectKind::Post | ObjectKind::Norm => Kind::Thread,
            ObjectKind::Comment => Kind::Comment,
        },
        author: PublicKey::parse(head.author.as_str())
            .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?,
    })
}

fn native_comment_scope(
    store: &DurableStore,
    head: &ObjectHead,
) -> Result<Option<CommentScope>, AppError> {
    if head.kind != ObjectKind::Comment || head.external_root.is_some() {
        return Ok(None);
    }
    let root_anchor = head.root.as_ref().ok_or(DomainError::InvalidObjectShape)?;
    let parent_anchor = head
        .parent
        .as_ref()
        .ok_or(DomainError::InvalidObjectShape)?;
    let root = store.state().heads.current_head(root_anchor)?;
    let parent = store.state().heads.current_head(parent_anchor)?;
    Ok(Some(CommentScope {
        root: event_reference(root)?,
        parent: event_reference(parent)?,
    }))
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        collections::BTreeMap,
        rc::Rc,
        sync::{Arc, Mutex},
    };

    use hydra_nostr::RelayDelivery;
    use hydra_store::EventLog;
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

    struct FailingSink;

    impl PersonaEventSink for FailingSink {
        fn append_persona(
            &mut self,
            _persona: Persona,
            _recorded_at: u64,
        ) -> Result<(), StoreError> {
            Err(StoreError::NotFound)
        }
    }

    #[derive(Default, Clone)]
    struct AcceptingPublisher(Arc<Mutex<Vec<String>>>);

    impl EventPublisher for AcceptingPublisher {
        fn publish<'a>(&'a self, event: &'a OutboundEvent) -> hydra_nostr::PublishFuture<'a> {
            let event_id = event.event_id.clone();
            let relays = event.relays.clone();
            let calls = self.0.clone();
            Box::pin(async move {
                calls.lock().unwrap().push(event_id);
                Ok(RelayDelivery {
                    accepted: relays,
                    rejected: Vec::new(),
                })
            })
        }
    }

    #[test]
    fn persona_creation_writes_public_event_and_keeps_secret_out_of_log() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let service = PersonaService::new(MemorySecrets::default());
        let persona = service.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let log = std::fs::read_to_string(root.path().join("events.jsonl")).unwrap();

        assert!(!persona.public_key.to_string().is_empty());
        assert!(!log.contains("Alice"));
        assert!(!log.contains("nsec"));
        drop(store);
        let reopened = DurableStore::open(root.path()).unwrap();
        assert_eq!(
            reopened
                .state()
                .personas
                .get(persona.id)
                .unwrap()
                .display_name,
            "Alice"
        );
    }

    #[test]
    fn persona_relay_preferences_are_signed_queued_and_persona_scoped() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let service = PersonaService::new(MemorySecrets::default());
        let persona = service.create(&mut store, "Alice".to_owned(), 10).unwrap();
        service
            .configure_relays(
                &mut store,
                persona.id,
                vec!["wss://read.example".to_owned()],
                vec!["wss://write.example".to_owned()],
                11,
            )
            .unwrap();

        assert_eq!(
            store.state().persona_relays.get(&persona.id),
            Some(&(
                vec!["wss://read.example".to_owned()],
                vec!["wss://write.example".to_owned()]
            ))
        );
        let outbound = store.state().outbound.values().next().unwrap();
        assert!(outbound.event_json.contains("wss://read.example"));
        assert!(outbound.event_json.contains("wss://write.example"));
        assert_eq!(outbound.relays.len(), 2);
    }

    #[test]
    fn persona_profile_is_standard_public_metadata_and_updates_local_identity() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let service = PersonaService::new(MemorySecrets::default());
        let persona = service.create(&mut store, "Alice".to_owned(), 10).unwrap();
        service
            .publish_profile(
                &mut store,
                persona.id,
                "Alice A.".to_owned(),
                None,
                &["wss://relay.example".to_owned()],
                11,
            )
            .unwrap();

        assert_eq!(
            store.state().personas.get(persona.id).unwrap().display_name,
            "Alice A."
        );
        let outbound = store.state().outbound.values().next().unwrap();
        let value: serde_json::Value = serde_json::from_str(&outbound.event_json).unwrap();
        assert_eq!(value["kind"], 0);
        assert!(value["content"].as_str().unwrap().contains("Alice A."));
    }

    #[test]
    fn verified_reddit_identity_proof_is_public_persona_state() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let service = PersonaService::new(MemorySecrets::default());
        let persona = service.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let proof = hydra_domain::RedditIdentityProof {
            persona: persona.id,
            username: "alice".to_owned(),
            artifact_url: "https://www.reddit.com/r/test/comments/abc/proof/def/".to_owned(),
            published_at: 11,
        };
        service
            .publish_reddit_identity_proof(
                &mut store,
                proof.clone(),
                &["wss://relay.example".to_owned()],
            )
            .unwrap();

        assert_eq!(
            store.state().reddit_identity_proofs.get(&persona.id),
            Some(&proof)
        );
        let outbound = store.state().outbound.values().next().unwrap();
        assert!(outbound.event_json.contains("reddit:alice"));
    }

    #[test]
    fn reddit_link_is_durable_and_remains_one_to_one() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let service = PersonaService::new(MemorySecrets::default());
        let alice = service.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let bob = service.create(&mut store, "Bob".to_owned(), 11).unwrap();
        let account = hydra_domain::RedditAccountId::parse("reddit-alice").unwrap();

        service
            .set_reddit_account(&mut store, alice.id, Some(account.clone()), 20)
            .unwrap();
        assert_eq!(
            store
                .state()
                .personas
                .get(alice.id)
                .unwrap()
                .reddit_account
                .as_ref(),
            Some(&account)
        );
        assert!(
            service
                .set_reddit_account(&mut store, bob.id, Some(account), 21)
                .is_err()
        );

        drop(store);
        let reopened = DurableStore::open(root.path()).unwrap();
        assert_eq!(
            reopened
                .state()
                .personas
                .get(alice.id)
                .unwrap()
                .reddit_account
                .as_ref()
                .unwrap()
                .as_str(),
            "reddit-alice"
        );
    }

    #[test]
    fn post_is_committed_locally_with_anchor_and_head_before_network() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        let head = DiscussionService::new(secrets)
            .create_post(
                &mut store,
                CreatePost {
                    persona_id: persona.id,
                    title: "Fungal networks".to_owned(),
                    link_url: None,
                    body: "Still worth discussing".to_owned(),
                    communities: vec![CommunityKey::parse("science").unwrap()],
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 20,
                },
            )
            .unwrap();

        assert_eq!(head.kind, ObjectKind::Post);
        assert_eq!(store.state().heads.current_count(), 1);
        assert_eq!(store.state().outbound.len(), 2);
        assert!(store.state().deliveries.is_empty());
    }

    #[test]
    fn link_post_keeps_a_safe_browser_url_and_allows_an_empty_body() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        let discussion = DiscussionService::new(secrets);
        let head = discussion
            .create_post(
                &mut store,
                CreatePost {
                    persona_id: persona.id,
                    title: "An external essay".to_owned(),
                    link_url: Some("https://example.com/essay".to_owned()),
                    body: String::new(),
                    communities: vec![CommunityKey::parse("science").unwrap()],
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 20,
                },
            )
            .unwrap();

        assert_eq!(head.body.as_str(), "");
        assert_eq!(
            head.external_source.unwrap().canonical,
            "https://example.com/essay"
        );
        assert!(store.state().outbound.values().any(|item| {
            item.event_json
                .contains("[\"r\",\"https://example.com/essay\"]")
        }));
        assert!(
            discussion
                .create_post(
                    &mut store,
                    CreatePost {
                        persona_id: persona.id,
                        title: "Unsafe".to_owned(),
                        link_url: Some("javascript:alert(1)".to_owned()),
                        body: String::new(),
                        communities: vec![CommunityKey::parse("science").unwrap()],
                        relays: Vec::new(),
                        recorded_at: 21,
                    },
                )
                .is_err()
        );
    }

    #[test]
    fn disowning_is_an_honest_nip_09_request_not_local_erasure() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        let discussion = DiscussionService::new(secrets);
        let head = discussion
            .create_post(
                &mut store,
                CreatePost {
                    persona_id: persona.id,
                    title: "Fungal networks".to_owned(),
                    link_url: None,
                    body: "Still worth discussing".to_owned(),
                    communities: vec![CommunityKey::parse("science").unwrap()],
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 20,
                },
            )
            .unwrap();

        discussion
            .request_disowning(
                &mut store,
                RequestObjectDisowning {
                    persona_id: persona.id,
                    anchor: head.anchor.clone(),
                    reason: "withdrawn".to_owned(),
                    relays: vec!["wss://relay.example".to_owned()],
                    requested_at: 21,
                },
            )
            .unwrap();

        assert!(store.state().heads.current_head(&head.anchor).is_ok());
        assert_eq!(
            store
                .state()
                .disowning_requests
                .get(&(persona.id, head.anchor)),
            Some(&"withdrawn".to_owned())
        );
    }

    #[test]
    fn remote_head_can_arrive_before_anchor_without_losing_the_edit() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let remote = Keys::generate();
        let communities = vec![CommunityKey::parse("science").unwrap()];
        let anchor =
            hydra_nostr::post_anchor(&remote, "Remote title", "Original", &communities, 10)
                .unwrap();
        let revision = hydra_nostr::object_head(
            &remote,
            EventReference {
                id: anchor.id,
                kind: anchor.kind,
                author: anchor.pubkey,
            },
            None,
            Some("Remote revised"),
            "Edited remotely",
            &communities,
            20,
        )
        .unwrap();
        let initial = hydra_nostr::received_object_head(&anchor, None)
            .unwrap()
            .unwrap();
        hydra_nostr::received_object_head(&revision, Some(&initial)).unwrap();

        assert!(
            ImportService::receive_public(&mut store, &revision.as_json(), 30)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.state().heads.current_count(), 0);
        assert_eq!(
            ImportService::receive_public(&mut store, &anchor.as_json(), 31)
                .unwrap()
                .len(),
            2
        );
        let current = store
            .state()
            .heads
            .current_head(&AnchorId::parse(anchor.id.to_hex()).unwrap())
            .unwrap();
        assert_eq!(current.body.as_str(), "Edited remotely");
        assert_eq!(current.title.as_deref(), Some("Remote revised"));
        assert_eq!(
            current.author.as_str(),
            remote.public_key().to_bech32().unwrap()
        );
        assert!(
            ImportService::receive_public(&mut store, &anchor.as_json(), 32)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.state().received_events.len(), 2);
    }

    #[test]
    fn remote_reaction_can_arrive_before_its_target_and_still_affect_feeds() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let author = Keys::generate();
        let voter = Keys::generate();
        let communities = vec![CommunityKey::parse("science").unwrap()];
        let anchor =
            hydra_nostr::post_anchor(&author, "Remote title", "Original", &communities, 10)
                .unwrap();
        let reaction = hydra_nostr::reaction(
            &voter,
            EventReference {
                id: anchor.id,
                kind: anchor.kind,
                author: anchor.pubkey,
            },
            "+",
            20,
        )
        .unwrap();

        ImportService::receive_public(&mut store, &reaction.as_json(), 30).unwrap();
        assert!(store.state().reactions.is_empty());
        ImportService::receive_public(&mut store, &anchor.as_json(), 31).unwrap();

        let voter = NostrPublicKey::parse(voter.public_key().to_bech32().unwrap()).unwrap();
        let anchor = AnchorId::parse(anchor.id.to_hex()).unwrap();
        assert_eq!(
            store.state().current_stance(&voter, &anchor),
            Some(&ReactionValue::Upvote)
        );
        assert_eq!(store.state().reactions.len(), 1);
    }

    #[test]
    fn hostile_public_event_shape_is_rejected_without_durable_side_effects() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let remote = Keys::generate();
        let event = hydra_nostr::post_anchor(
            &remote,
            "Remote title",
            "Original",
            &[CommunityKey::parse("science").unwrap()],
            10,
        )
        .unwrap();
        let mut value = serde_json::from_str::<serde_json::Value>(&event.as_json()).unwrap();
        value["tags"] = serde_json::Value::Array(
            (0..257)
                .map(|_| serde_json::json!(["t", "science"]))
                .collect(),
        );

        assert!(ImportService::receive_public(&mut store, &value.to_string(), 30).is_err());
        assert!(store.state().received_events.is_empty());
        assert_eq!(
            EventLog::open(root.path())
                .unwrap()
                .read_all()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn future_signed_schema_is_preserved_raw_without_materialization_or_poisoning() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let remote = Keys::generate();
        let voter = Keys::generate();
        let communities = [CommunityKey::parse("science").unwrap()];
        let anchor =
            hydra_nostr::post_anchor(&remote, "Remote title", "Original", &communities, 10)
                .unwrap();
        ImportService::receive_public(&mut store, &anchor.as_json(), 20).unwrap();

        let anchor_id = anchor.id.to_hex();
        let address = format!("hydra:head:{anchor_id}");
        let future_builder =
            EventBuilder::new(Kind::Custom(hydra_nostr::OBJECT_HEAD_KIND), "Future edit")
                .tags(vec![
                    nostr::Tag::parse(["d", address.as_str()]).unwrap(),
                    nostr::Tag::parse(["e", anchor_id.as_str()]).unwrap(),
                    nostr::Tag::parse(["k", "11"]).unwrap(),
                    nostr::Tag::parse(["L", "hydra"]).unwrap(),
                    nostr::Tag::parse(["l", "object-head", "hydra"]).unwrap(),
                    nostr::Tag::parse(["version", "hydra-protocol/v999"]).unwrap(),
                    nostr::Tag::parse(["t", "science"]).unwrap(),
                ])
                .custom_created_at(nostr::Timestamp::from(21));
        let future = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(future_builder.sign(&remote))
            .unwrap();

        assert!(ImportService::receive_public(&mut store, &future.as_json(), 21).is_err());
        assert_eq!(store.state().received_events.len(), 2);
        assert_eq!(
            store
                .state()
                .heads
                .current_head(&AnchorId::parse(anchor_id.clone()).unwrap())
                .unwrap()
                .body
                .as_str(),
            "Original"
        );

        let reaction = hydra_nostr::reaction(
            &voter,
            EventReference {
                id: anchor.id,
                kind: anchor.kind,
                author: anchor.pubkey,
            },
            "+",
            22,
        )
        .unwrap();
        ImportService::receive_public(&mut store, &reaction.as_json(), 22).unwrap();
        assert_eq!(store.state().reactions.len(), 1);
    }

    #[test]
    fn verified_canon_records_are_retained_as_evidence_without_hydra_heads() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let keys = Keys::generate();
        let canon = EventBuilder::new(
            Kind::Custom(hydra_nostr::CANON_RECORD_KIND),
            r#"{"id":"work-1","title":"The Dispossessed"}"#,
        )
        .tags([
            nostr::Tag::parse(["d", "dev.wizardry.canon:work:work-1"]).unwrap(),
            nostr::Tag::parse(["L", hydra_nostr::CANON_NAMESPACE]).unwrap(),
            nostr::Tag::parse(["l", "work", hydra_nostr::CANON_NAMESPACE]).unwrap(),
            nostr::Tag::parse(["version", hydra_nostr::CANON_SCHEMA_VERSION]).unwrap(),
            nostr::Tag::parse(["i", "isbn:9780061054884"]).unwrap(),
        ])
        .custom_created_at(nostr::Timestamp::from(10))
        .sign_with_keys(&keys)
        .unwrap();

        let heads = ImportService::receive_public(&mut store, &canon.as_json(), 11).unwrap();
        assert!(heads.is_empty());
        assert!(
            store
                .state()
                .received_events
                .contains_key(&canon.id.to_hex())
        );
        assert_eq!(store.state().heads.current_count(), 0);
    }

    #[test]
    fn nested_comment_and_edits_keep_stable_anchors_and_topology() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        let service = DiscussionService::new(secrets.clone());
        let post = service
            .create_post(
                &mut store,
                CreatePost {
                    persona_id: persona.id,
                    title: "Fungal networks".to_owned(),
                    link_url: None,
                    body: "Root".to_owned(),
                    communities: vec![CommunityKey::parse("science").unwrap()],
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 20,
                },
            )
            .unwrap();
        let comment = service
            .create_comment(
                &mut store,
                CreateComment {
                    persona_id: persona.id,
                    parent_anchor: post.anchor.clone(),
                    body: "First reply".to_owned(),
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 30,
                },
            )
            .unwrap();
        let edited = service
            .edit_object(
                &mut store,
                EditObject {
                    persona_id: persona.id,
                    anchor: comment.anchor.clone(),
                    title: Some("ignored on comments".to_owned()),
                    body: "Clearer reply".to_owned(),
                    communities: None,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 30,
                },
            )
            .unwrap();

        assert_eq!(edited.anchor, comment.anchor);
        assert_eq!(edited.root.as_ref(), Some(&post.anchor));
        assert_eq!(edited.parent.as_ref(), Some(&post.anchor));
        assert_eq!(edited.title, None);
        assert_eq!(edited.body.as_str(), "Clearer reply");
        assert!(edited.edited_at > comment.edited_at);
        assert_eq!(store.state().heads.current_count(), 2);
        assert_eq!(store.state().outbound.len(), 5);
    }

    #[test]
    fn editing_a_post_replaces_community_tags_without_splitting_its_lineage() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        let service = DiscussionService::new(secrets);
        let post = service
            .create_post(
                &mut store,
                CreatePost {
                    persona_id: persona.id,
                    title: "Fungal networks".to_owned(),
                    link_url: None,
                    body: "Root".to_owned(),
                    communities: vec![CommunityKey::parse("science").unwrap()],
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 20,
                },
            )
            .unwrap();

        let edited = service
            .edit_object(
                &mut store,
                EditObject {
                    persona_id: persona.id,
                    anchor: post.anchor.clone(),
                    title: Some("Fungal networks, revisited".to_owned()),
                    body: "One discussion, two coordinates".to_owned(),
                    communities: Some(vec![
                        CommunityKey::parse("biology").unwrap(),
                        CommunityKey::parse("mycology").unwrap(),
                    ]),
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 30,
                },
            )
            .unwrap();

        assert_eq!(edited.anchor, post.anchor);
        assert_eq!(edited.communities.len(), 2);
        assert_eq!(edited.communities[0].as_str(), "biology");
        assert_eq!(edited.communities[1].as_str(), "mycology");
        assert_eq!(store.state().heads.current_count(), 1);
    }

    #[test]
    fn another_persona_cannot_edit_an_object() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let personas = PersonaService::new(secrets.clone());
        let alice = personas.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let bob = personas.create(&mut store, "Bob".to_owned(), 11).unwrap();
        let service = DiscussionService::new(secrets);
        let post = service
            .create_post(
                &mut store,
                CreatePost {
                    persona_id: alice.id,
                    title: "Fungal networks".to_owned(),
                    link_url: None,
                    body: "Root".to_owned(),
                    communities: vec![CommunityKey::parse("science").unwrap()],
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 20,
                },
            )
            .unwrap();

        let result = service.edit_object(
            &mut store,
            EditObject {
                persona_id: bob.id,
                anchor: post.anchor,
                title: None,
                body: "Hijacked".to_owned(),
                communities: None,
                relays: vec!["wss://relay.example".to_owned()],
                recorded_at: 30,
            },
        );
        assert!(matches!(result, Err(AppError::NotObjectAuthor)));
    }

    #[test]
    fn revotes_preserve_history_and_credit_only_durable_reaffirmations() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        let service = DiscussionService::new(secrets);
        let post = service
            .create_post(
                &mut store,
                CreatePost {
                    persona_id: persona.id,
                    title: "Fungal networks".to_owned(),
                    link_url: None,
                    body: "Root".to_owned(),
                    communities: vec![CommunityKey::parse("science").unwrap()],
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 20,
                },
            )
            .unwrap();
        let vote = |store: &mut DurableStore, value, at| {
            service
                .react(
                    store,
                    ReactToObject {
                        persona_id: persona.id,
                        target: post.anchor.clone(),
                        value,
                        relays: vec!["wss://relay.example".to_owned()],
                        recorded_at: at,
                    },
                )
                .unwrap()
        };

        assert!(!vote(&mut store, ReactionValue::Upvote, 100).credited_reaffirmation);
        assert!(!vote(&mut store, ReactionValue::Neutral, 101).credited_reaffirmation);
        assert!(!vote(&mut store, ReactionValue::Upvote, 200).credited_reaffirmation);
        assert!(vote(&mut store, ReactionValue::Upvote, 100 + 18 * 60 * 60).credited_reaffirmation);
        assert_eq!(store.state().reactions.len(), 4);
        assert_eq!(
            store
                .state()
                .current_stance(&persona.public_key, &post.anchor),
            Some(&ReactionValue::Upvote)
        );
    }

    #[test]
    fn revisit_is_private_durable_state_without_an_outbound_event() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        let service = DiscussionService::new(secrets.clone());
        let post = service
            .create_post(
                &mut store,
                CreatePost {
                    persona_id: persona.id,
                    title: "Fungal networks".to_owned(),
                    link_url: None,
                    body: "Root".to_owned(),
                    communities: vec![CommunityKey::parse("science").unwrap()],
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 20,
                },
            )
            .unwrap();
        let outbound_before = store.state().outbound.len();

        service
            .set_revisit(
                &mut store,
                SetRevisit {
                    persona_id: persona.id,
                    target: post.anchor.clone(),
                    intent: RevisitIntent::Study,
                    due_at: Some(1_000),
                    recorded_at: 30,
                },
            )
            .unwrap();

        assert_eq!(store.state().outbound.len(), outbound_before);
        assert_eq!(
            private_state(&secrets, &store, persona.id)
                .unwrap()
                .revisits
                .len(),
            1
        );

        service
            .remove_revisit(
                &mut store,
                RemoveRevisit {
                    persona_id: persona.id,
                    target: post.anchor,
                    recorded_at: 40,
                },
            )
            .unwrap();
        assert!(
            private_state(&secrets, &store, persona.id)
                .unwrap()
                .revisits
                .is_empty()
        );
        assert_eq!(store.state().outbound.len(), outbound_before);
    }

    #[test]
    fn remote_projection_records_are_verified_and_materialized() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let remote = Keys::generate();
        let public_key = NostrPublicKey::parse(remote.public_key().to_bech32().unwrap()).unwrap();

        let anchor_event = hydra_nostr::post_anchor(
            &remote,
            "Remote projection",
            "Body",
            &[CommunityKey::parse("science").unwrap()],
            22,
        )
        .unwrap();
        let mut projection = Projection {
            id: hydra_domain::ProjectionId::new(),
            anchor: AnchorId::parse(anchor_event.id.to_hex()).unwrap(),
            destination: ExternalId::new("reddit-community", "science").unwrap(),
            external_id: Some(ExternalId::new("reddit", "t3_example").unwrap()),
            external_url: Some(
                "https://www.reddit.com/r/science/comments/example/title/".to_owned(),
            ),
            persona: PersonaId::new(),
            state: hydra_domain::ProjectionState::Live,
            sync_enabled: true,
            payload_hash: None,
            last_synced_head: None,
            rendered_payload: None,
            rendered_suffix: None,
            formatting_losses: Vec::new(),
            last_attempt_at: None,
            last_success_at: None,
            divergence: None,
            display_error: None,
        };
        let projection_event = hydra_nostr::projection_record(&remote, &projection, 23).unwrap();
        ImportService::receive_public(&mut store, &projection_event.as_json(), 24).unwrap();
        assert!(store.state().public_projections.is_empty());
        ImportService::receive_public(&mut store, &anchor_event.as_json(), 25).unwrap();
        assert_eq!(store.state().public_projections.len(), 1);
        let received = store.state().public_projections.values().next().unwrap();
        assert_eq!(received.author, public_key);
        assert_eq!(received.reddit_fullname, "t3_example");
        assert_eq!(received.target_subreddit, "science");

        projection.state = hydra_domain::ProjectionState::Removed;
        let replacement = hydra_nostr::projection_record(&remote, &projection, 26).unwrap();
        ImportService::receive_public(&mut store, &replacement.as_json(), 27).unwrap();
        assert_eq!(store.state().public_projections.len(), 1);
        assert_eq!(
            store
                .state()
                .public_projections
                .values()
                .next()
                .unwrap()
                .state,
            "removed"
        );

        let impostor = Keys::generate();
        let forged = hydra_nostr::projection_record(&impostor, &projection, 28).unwrap();
        assert!(ImportService::receive_public(&mut store, &forged.as_json(), 29).is_err());
        assert_eq!(
            store
                .state()
                .public_projections
                .values()
                .next()
                .unwrap()
                .author,
            public_key
        );
    }

    #[test]
    fn projection_history_rejects_silent_restoration_after_withdrawal() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        let post = DiscussionService::new(secrets.clone())
            .create_post(
                &mut store,
                CreatePost {
                    persona_id: persona.id,
                    title: "Fungal networks".to_owned(),
                    link_url: None,
                    body: "Root".to_owned(),
                    communities: vec![CommunityKey::parse("science").unwrap()],
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 20,
                },
            )
            .unwrap();
        let service = ProjectionService::new(secrets);
        let mut projection = Projection {
            id: hydra_domain::ProjectionId::new(),
            anchor: post.anchor,
            destination: ExternalId::new("example", "destination").unwrap(),
            external_id: None,
            external_url: None,
            persona: persona.id,
            state: hydra_domain::ProjectionState::Queued,
            sync_enabled: true,
            payload_hash: None,
            last_synced_head: None,
            rendered_payload: None,
            rendered_suffix: None,
            formatting_losses: Vec::new(),
            last_attempt_at: None,
            last_success_at: None,
            divergence: None,
            display_error: None,
        };
        let relays = vec!["wss://relay.example".to_owned()];
        service
            .record(&mut store, projection.clone(), relays.clone(), 30)
            .unwrap();
        for state in [
            hydra_domain::ProjectionState::Submitting,
            hydra_domain::ProjectionState::Live,
            hydra_domain::ProjectionState::Withdrawn,
        ] {
            projection.transition(state).unwrap();
            service
                .record(&mut store, projection.clone(), relays.clone(), 31)
                .unwrap();
        }
        projection.state = hydra_domain::ProjectionState::Live;

        let result = service.record(&mut store, projection, relays, 40);
        assert!(matches!(
            result,
            Err(AppError::Store(StoreError::Domain(_)))
        ));
        assert_eq!(
            store.state().projections.values().next().unwrap().state,
            hydra_domain::ProjectionState::Withdrawn
        );
    }

    #[test]
    fn private_social_state_stays_local_and_public_state_uses_standard_lists() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let personas = PersonaService::new(secrets.clone());
        let alice = personas.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let bob = personas.create(&mut store, "Bob".to_owned(), 11).unwrap();
        let service = SocialService::new(secrets);
        let initial_outbound = store.state().outbound.len();

        service
            .set_follow(
                &mut store,
                &SetFollow {
                    persona_id: alice.id,
                    target: bob.public_key.clone(),
                    public: false,
                    following: true,
                    relays: vec!["wss://relay.example".to_owned()],
                    changed_at: 20,
                },
            )
            .unwrap();
        assert_eq!(store.state().outbound.len(), initial_outbound);

        service
            .set_follow(
                &mut store,
                &SetFollow {
                    persona_id: alice.id,
                    target: bob.public_key.clone(),
                    public: true,
                    following: true,
                    relays: vec!["wss://relay.example".to_owned()],
                    changed_at: 21,
                },
            )
            .unwrap();
        assert_eq!(store.state().outbound.len(), initial_outbound + 2);

        service
            .publish_follow_set(
                &mut store,
                &PublishFollowSet {
                    persona_id: alice.id,
                    identifier: "trusted".to_owned(),
                    title: "Trusted personas".to_owned(),
                    members: vec![bob.public_key.clone()],
                    relays: vec!["wss://relay.example".to_owned()],
                    published_at: 22,
                },
            )
            .unwrap();
        assert_eq!(store.state().public_follow_sets.len(), 1);
        assert_eq!(store.state().outbound.len(), initial_outbound + 3);

        service
            .set_block(
                &mut store,
                &SetBlock {
                    persona_id: alice.id,
                    target: bob.public_key.clone(),
                    public: true,
                    blocked: true,
                    reason: Some("Persistent unwanted contact".to_owned()),
                    relays: vec!["wss://relay.example".to_owned()],
                    changed_at: 22,
                },
            )
            .unwrap();
        assert_eq!(store.state().outbound.len(), initial_outbound + 5);
        let block = store
            .state()
            .blocks
            .get(&(alice.id, bob.public_key.clone()))
            .unwrap();
        assert!(block.blocked);
        assert_eq!(block.reason.as_deref(), Some("Persistent unwanted contact"));
    }

    #[test]
    fn private_social_graph_is_encrypted_in_the_inspectable_arche() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let personas = PersonaService::new(secrets.clone());
        let alice = personas.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let bob = personas.create(&mut store, "Bob".to_owned(), 11).unwrap();
        let service = SocialService::new(secrets.clone());
        let secret_reason = "private-reason-never-readable";

        service
            .set_block(
                &mut store,
                &SetBlock {
                    persona_id: alice.id,
                    target: bob.public_key.clone(),
                    public: false,
                    blocked: true,
                    reason: Some(secret_reason.to_owned()),
                    relays: vec!["wss://relay.example".to_owned()],
                    changed_at: 20,
                },
            )
            .unwrap();

        let arche = std::fs::read_to_string(root.path().join("events.jsonl")).unwrap();
        assert!(!arche.contains(secret_reason));
        let state = private_state(&secrets, &store, alice.id).unwrap();
        assert_eq!(
            state
                .blocks
                .get(&bob.public_key)
                .and_then(|block| block.reason.as_deref()),
            Some(secret_reason)
        );
    }

    #[test]
    fn flocking_blocks_are_scoped_reversible_and_source_configured() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let personas = PersonaService::new(secrets.clone());
        let alice = personas.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let bob = personas.create(&mut store, "Bob".to_owned(), 11).unwrap();
        let carol = personas.create(&mut store, "Carol".to_owned(), 12).unwrap();
        let service = SocialService::new(secrets.clone());
        let science = CommunityKey::parse("science").unwrap();
        let reason = "private-scoped-reason";

        service
            .set_person_judgment(
                &mut store,
                &SetPersonJudgment {
                    persona_id: alice.id,
                    target: bob.public_key.clone(),
                    topic: Some(science.clone()),
                    public: false,
                    faculty: flocking_core::Faculty::Block,
                    action: flocking_core::Action::Block,
                    reason: Some(reason.to_owned()),
                    relays: vec!["wss://relay.example".to_owned()],
                    changed_at: 20,
                },
            )
            .unwrap();
        service
            .set_person_source(
                &mut store,
                &SetPersonSource {
                    persona_id: alice.id,
                    source: carol.public_key.clone(),
                    faculty: flocking_core::Faculty::Block,
                    global: true,
                    topics: BTreeSet::from([science.clone()]),
                    rank: Some(1),
                    enabled: true,
                    completeness: flocking_core::Completeness::Unknown,
                    changed_at: 21,
                },
            )
            .unwrap();

        let private = private_state(&secrets, &store, alice.id).unwrap();
        let current = flocking_core::canonical_current(
            &private
                .flocking_judgments
                .values()
                .map(|record| record.judgment.clone())
                .collect::<Vec<_>>(),
        );
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].scope.to_string(), "topic:science");
        assert_eq!(current[0].reason.as_deref(), Some(reason));
        let profile = private.flocking_profile.as_ref().unwrap();
        assert!(
            profile
                .config
                .grant(
                    &hydra_nostr::flocking_public_key(&carol.public_key).unwrap(),
                    flocking_core::Faculty::Block,
                )
                .is_some()
        );
        assert_eq!(profile.source_states.len(), 2);
        assert!(
            !std::fs::read_to_string(root.path().join("events.jsonl"))
                .unwrap()
                .contains(reason)
        );

        service
            .set_person_judgment(
                &mut store,
                &SetPersonJudgment {
                    persona_id: alice.id,
                    target: bob.public_key,
                    topic: Some(science),
                    public: false,
                    faculty: flocking_core::Faculty::Block,
                    action: flocking_core::Action::Unblock,
                    reason: None,
                    relays: Vec::new(),
                    changed_at: 22,
                },
            )
            .unwrap();
        let private = private_state(&secrets, &store, alice.id).unwrap();
        let current = flocking_core::canonical_current(
            &private
                .flocking_judgments
                .values()
                .map(|record| record.judgment.clone())
                .collect::<Vec<_>>(),
        );
        assert_eq!(current[0].action, flocking_core::Action::Unblock);
    }

    #[test]
    fn follow_pin_reverse_sources_and_rescue_share_one_persona_configuration() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let personas = PersonaService::new(secrets.clone());
        let alice = personas.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let bob = personas.create(&mut store, "Bob".to_owned(), 11).unwrap();
        let service = SocialService::new(secrets.clone());
        let science = CommunityKey::parse("science").unwrap();

        service
            .set_person_source(
                &mut store,
                &SetPersonSource {
                    persona_id: alice.id,
                    source: bob.public_key.clone(),
                    faculty: flocking_core::Faculty::Follow,
                    global: true,
                    topics: BTreeSet::new(),
                    rank: Some(1),
                    enabled: true,
                    completeness: flocking_core::Completeness::Complete,
                    changed_at: 20,
                },
            )
            .unwrap();
        service
            .set_person_source(
                &mut store,
                &SetPersonSource {
                    persona_id: alice.id,
                    source: bob.public_key.clone(),
                    faculty: flocking_core::Faculty::Pin,
                    global: false,
                    topics: BTreeSet::from([science.clone()]),
                    rank: None,
                    enabled: true,
                    completeness: flocking_core::Completeness::Complete,
                    changed_at: 21,
                },
            )
            .unwrap();
        service
            .set_reverse_block_source(
                &mut store,
                &SetReverseBlockSource {
                    persona_id: alice.id,
                    source: bob.public_key.clone(),
                    global: false,
                    topics: BTreeSet::from([science.clone()]),
                    enabled: true,
                    completeness: flocking_core::Completeness::Complete,
                    changed_at: 22,
                },
            )
            .unwrap();
        service
            .rescue_person(
                &mut store,
                &RescuePerson {
                    persona_id: alice.id,
                    target: bob.public_key.clone(),
                    topic: Some(science),
                    public: false,
                    relays: Vec::new(),
                    changed_at: 23,
                },
            )
            .unwrap();

        let private = private_state(&secrets, &store, alice.id).unwrap();
        let profile = private.flocking_profile.as_ref().unwrap();
        let source = profile.config.sources.first().unwrap();
        assert!(
            source
                .grants
                .iter()
                .any(|grant| grant.faculty == flocking_core::Faculty::Follow)
        );
        assert!(
            source
                .grants
                .iter()
                .any(|grant| grant.faculty == flocking_core::Faculty::Pin)
        );
        assert!(source.reverse_blocks.is_some());
        assert!(private.follows.get(&bob.public_key).unwrap().following);
        assert!(private.flocking_judgments.values().any(|record| {
            record.judgment.faculty == flocking_core::Faculty::Block
                && record.judgment.action == flocking_core::Action::Unblock
        }));
    }

    #[test]
    fn public_flocking_block_queues_canonical_and_nip51_events() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let personas = PersonaService::new(secrets.clone());
        let alice = personas.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let bob = personas.create(&mut store, "Bob".to_owned(), 11).unwrap();
        let service = SocialService::new(secrets);
        let outbound_before = store.state().outbound.len();
        service
            .set_person_judgment(
                &mut store,
                &SetPersonJudgment {
                    persona_id: alice.id,
                    target: bob.public_key,
                    topic: None,
                    public: true,
                    faculty: flocking_core::Faculty::Block,
                    action: flocking_core::Action::Block,
                    reason: Some("Public reason".to_owned()),
                    relays: vec!["wss://relay.example".to_owned()],
                    changed_at: 20,
                },
            )
            .unwrap();
        let new_kinds = store
            .state()
            .outbound
            .values()
            .skip(outbound_before)
            .map(|outbound| Event::from_json(&outbound.event_json).unwrap().kind)
            .collect::<BTreeSet<_>>();
        assert_eq!(store.state().outbound.len(), outbound_before + 2);
        assert!(new_kinds.contains(&Kind::Custom(flocking_core::JUDGMENT_KIND)));
        assert!(new_kinds.contains(&Kind::Custom(10_000)));
    }

    #[test]
    fn silence_keeps_one_cutoff_until_unsilenced_and_publishes_only_canonical_events() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let personas = PersonaService::new(secrets.clone());
        let alice = personas.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let bob = personas.create(&mut store, "Bob".to_owned(), 11).unwrap();
        let service = SocialService::new(secrets.clone());
        let request = |action, changed_at| SetPersonJudgment {
            persona_id: alice.id,
            target: bob.public_key.clone(),
            topic: None,
            public: false,
            faculty: flocking_core::Faculty::Silence,
            action,
            reason: None,
            relays: Vec::new(),
            changed_at,
        };

        service
            .set_person_judgment(&mut store, &request(flocking_core::Action::Silence, 20))
            .unwrap();
        service
            .set_person_judgment(&mut store, &request(flocking_core::Action::Silence, 30))
            .unwrap();
        let private = private_state(&secrets, &store, alice.id).unwrap();
        let current = flocking_core::canonical_current(
            &private
                .flocking_judgments
                .values()
                .map(|record| record.judgment.clone())
                .collect::<Vec<_>>(),
        );
        assert_eq!(current[0].since, Some(20));

        service
            .set_person_judgment(&mut store, &request(flocking_core::Action::Unsilence, 40))
            .unwrap();
        service
            .set_person_judgment(&mut store, &request(flocking_core::Action::Silence, 50))
            .unwrap();
        let private = private_state(&secrets, &store, alice.id).unwrap();
        let current = flocking_core::canonical_current(
            &private
                .flocking_judgments
                .values()
                .map(|record| record.judgment.clone())
                .collect::<Vec<_>>(),
        );
        assert_eq!(current[0].since, Some(50));

        let outbound_before = store.state().outbound.len();
        let mut public = request(flocking_core::Action::Silence, 60);
        public.public = true;
        public.relays = vec!["wss://relay.example".to_owned()];
        service.set_person_judgment(&mut store, &public).unwrap();
        let canonical_count = store
            .state()
            .outbound
            .values()
            .map(|outbound| Event::from_json(&outbound.event_json).unwrap().kind)
            .filter(|kind| *kind == Kind::Custom(flocking_core::JUDGMENT_KIND))
            .count();
        assert_eq!(store.state().outbound.len(), outbound_before + 1);
        assert_eq!(canonical_count, 1);
    }

    #[test]
    fn content_judgments_use_stable_anchors_and_keep_removal_topic_scoped() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let personas = PersonaService::new(secrets.clone());
        let alice = personas.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let science = CommunityKey::parse("science").unwrap();
        let post = DiscussionService::new(secrets.clone())
            .create_post(
                &mut store,
                CreatePost {
                    persona_id: alice.id,
                    title: "One logical object".to_owned(),
                    link_url: None,
                    body: "The judgment follows this anchor across revisions.".to_owned(),
                    communities: vec![science.clone()],
                    relays: Vec::new(),
                    recorded_at: 11,
                },
            )
            .unwrap();
        let service = SocialService::new(secrets.clone());
        service
            .set_content_judgment(
                &mut store,
                &SetContentJudgment {
                    persona_id: alice.id,
                    target: post.anchor.clone(),
                    topic: None,
                    public: false,
                    faculty: flocking_core::Faculty::Hide,
                    action: flocking_core::Action::Hide,
                    reason: Some("Not useful to me".to_owned()),
                    relays: Vec::new(),
                    changed_at: 12,
                },
            )
            .unwrap();
        let private = private_state(&secrets, &store, alice.id).unwrap();
        let hide = private
            .flocking_judgments
            .values()
            .find(|record| record.judgment.faculty == flocking_core::Faculty::Hide)
            .unwrap();
        assert_eq!(hide.judgment.scope, flocking_core::Scope::Global);
        assert_eq!(
            hide.judgment.target,
            flocking_core::Target::Content(flocking_core::ContentTarget::Event(
                flocking_core::EventId::parse(post.anchor.as_str()).unwrap()
            ))
        );

        let outbound_before = store.state().outbound.len();
        service
            .set_content_judgment(
                &mut store,
                &SetContentJudgment {
                    persona_id: alice.id,
                    target: post.anchor.clone(),
                    topic: Some(science),
                    public: true,
                    faculty: flocking_core::Faculty::CommunityMembership,
                    action: flocking_core::Action::Remove,
                    reason: Some("Off topic".to_owned()),
                    relays: vec!["wss://relay.example".to_owned()],
                    changed_at: 13,
                },
            )
            .unwrap();
        assert_eq!(store.state().outbound.len(), outbound_before + 1);
        let removal = store
            .state()
            .flocking_judgments
            .iter()
            .find(|judgment| judgment.faculty == flocking_core::Faculty::CommunityMembership)
            .unwrap();
        assert_eq!(removal.scope.to_string(), "topic:science");

        let error = service
            .set_content_judgment(
                &mut store,
                &SetContentJudgment {
                    persona_id: alice.id,
                    target: post.anchor,
                    topic: None,
                    public: false,
                    faculty: flocking_core::Faculty::CommunityMembership,
                    action: flocking_core::Action::Remove,
                    reason: None,
                    relays: Vec::new(),
                    changed_at: 14,
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("Flocking"));
    }

    #[test]
    fn local_filters_are_encrypted_and_never_cross_persona_boundaries() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let personas = PersonaService::new(secrets.clone());
        let alice = personas.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let bob = personas.create(&mut store, "Bob".to_owned(), 11).unwrap();
        let service = SocialService::new(secrets.clone());
        let private_word = "ciphertext-only-filter-term";

        service
            .set_local_filter(
                &mut store,
                &SetLocalFilter {
                    persona_id: alice.id,
                    kind: LocalFilterKind::Word,
                    value: private_word.to_owned(),
                    enabled: true,
                    changed_at: 20,
                },
            )
            .unwrap();

        let arche = std::fs::read_to_string(root.path().join("events.jsonl")).unwrap();
        assert!(!arche.contains(private_word));
        assert!(
            private_state(&secrets, &store, alice.id)
                .unwrap()
                .filters
                .contains_key(&(LocalFilterKind::Word, private_word.to_owned()))
        );
        assert!(
            private_state(&secrets, &store, bob.id)
                .unwrap()
                .filters
                .is_empty()
        );
        DiscussionService::new(secrets.clone())
            .create_post(
                &mut store,
                CreatePost {
                    persona_id: bob.id,
                    title: private_word.to_owned(),
                    link_url: None,
                    body: "Filtered locally".to_owned(),
                    communities: vec![CommunityKey::parse("science").unwrap()],
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 21,
                },
            )
            .unwrap();
        let alice_private = private_state(&secrets, &store, alice.id).unwrap();
        assert!(
            FeedService::feed(
                &store,
                &alice_private,
                alice.id,
                &hydra_store::Settings::default(),
                FeedLens::New,
            )
            .is_empty()
        );
    }

    #[test]
    fn community_subscriptions_use_public_interests_or_private_envelopes() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        let service = SocialService::new(secrets.clone());
        let science = CommunityKey::parse("science").unwrap();
        let private_topic = CommunityKey::parse("private_topic").unwrap();

        service
            .set_community_subscription(
                &mut store,
                &SetCommunitySubscription {
                    persona_id: persona.id,
                    community: science.clone(),
                    public: true,
                    subscribed: true,
                    relays: vec!["wss://relay.example".to_owned()],
                    changed_at: 20,
                },
            )
            .unwrap();
        service
            .set_community_subscription(
                &mut store,
                &SetCommunitySubscription {
                    persona_id: persona.id,
                    community: private_topic.clone(),
                    public: false,
                    subscribed: true,
                    relays: vec!["wss://relay.example".to_owned()],
                    changed_at: 21,
                },
            )
            .unwrap();

        assert!(
            store
                .state()
                .subscriptions
                .get(&(persona.id, science))
                .unwrap()
                .subscribed
        );
        assert!(
            private_state(&secrets, &store, persona.id)
                .unwrap()
                .subscriptions
                .get(&private_topic)
                .unwrap()
                .subscribed
        );
        let arche = std::fs::read_to_string(root.path().join("events.jsonl")).unwrap();
        assert!(!arche.contains("private_topic"));
    }

    #[test]
    fn feed_lenses_are_local_transparent_and_deterministic() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let personas = PersonaService::new(secrets.clone());
        let alice = personas.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let bob = personas.create(&mut store, "Bob".to_owned(), 11).unwrap();
        let discussion = DiscussionService::new(secrets.clone());
        let science = CommunityKey::parse("science").unwrap();
        let bob_post = discussion
            .create_post(
                &mut store,
                CreatePost {
                    persona_id: bob.id,
                    title: "Bob first".to_owned(),
                    link_url: None,
                    body: "Older".to_owned(),
                    communities: vec![science.clone()],
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 20,
                },
            )
            .unwrap();
        let alice_post = discussion
            .create_post(
                &mut store,
                CreatePost {
                    persona_id: alice.id,
                    title: "Alice later".to_owned(),
                    link_url: None,
                    body: "Newer".to_owned(),
                    communities: vec![science.clone()],
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 30,
                },
            )
            .unwrap();
        let social = SocialService::new(secrets.clone());
        social
            .set_follow(
                &mut store,
                &SetFollow {
                    persona_id: alice.id,
                    target: bob.public_key.clone(),
                    public: false,
                    following: true,
                    relays: vec!["wss://relay.example".to_owned()],
                    changed_at: 31,
                },
            )
            .unwrap();
        social
            .set_community_subscription(
                &mut store,
                &SetCommunitySubscription {
                    persona_id: alice.id,
                    community: science.clone(),
                    public: false,
                    subscribed: true,
                    relays: vec!["wss://relay.example".to_owned()],
                    changed_at: 32,
                },
            )
            .unwrap();
        discussion
            .react(
                &mut store,
                ReactToObject {
                    persona_id: alice.id,
                    target: bob_post.anchor.clone(),
                    value: ReactionValue::Upvote,
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 33,
                },
            )
            .unwrap();
        let private = private_state(&secrets, &store, alice.id).unwrap();

        let settings = hydra_store::Settings::default();
        let new = FeedService::community(
            &store,
            &private,
            alice.id,
            &settings,
            &science,
            FeedLens::New,
        );
        assert_eq!(new[0].anchor, alice_post.anchor);
        let top = FeedService::community(
            &store,
            &private,
            alice.id,
            &settings,
            &science,
            FeedLens::Top,
        );
        assert_eq!(top[0].anchor, bob_post.anchor);
        assert_eq!(
            FeedService::my_feed(&store, &private, alice.id, &settings).len(),
            2
        );
    }

    #[tokio::test]
    async fn direct_messages_are_recoverable_encrypted_and_request_filtered() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let personas = PersonaService::new(secrets.clone());
        let alice = personas.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let bob = personas.create(&mut store, "Bob".to_owned(), 11).unwrap();
        let messaging = MessagingService::new(secrets.clone());
        let message = messaging
            .send(
                &mut store,
                SendDirectMessage {
                    persona_id: alice.id,
                    recipient: bob.public_key.clone(),
                    body: "not-readable-in-the-arche".to_owned(),
                    recipient_relays: vec!["wss://bob-inbox.example".to_owned()],
                    sender_relays: vec!["wss://alice-inbox.example".to_owned()],
                    created_at: 20,
                },
            )
            .await
            .unwrap();

        assert_eq!(store.state().outbound.len(), 2);
        assert_eq!(
            private_state(&secrets, &store, alice.id).unwrap().messages,
            vec![message]
        );
        let arche = std::fs::read_to_string(root.path().join("events.jsonl")).unwrap();
        assert!(!arche.contains("not-readable-in-the-arche"));

        let bob_hex = PublicKey::parse(bob.public_key.as_str()).unwrap().to_hex();
        let recipient_wrap = store
            .state()
            .outbound
            .values()
            .find(|event| event.event_json.contains(&bob_hex))
            .unwrap()
            .event_json
            .clone();
        let received = messaging
            .receive(&mut store, bob.id, &recipient_wrap, 21)
            .await
            .unwrap();
        assert!(received.request);
        assert_eq!(received.body, "not-readable-in-the-arche");
        assert_eq!(
            messaging
                .receive(&mut store, bob.id, &recipient_wrap, 22)
                .await
                .unwrap(),
            received
        );
        assert_eq!(
            private_state(&secrets, &store, bob.id)
                .unwrap()
                .messages
                .len(),
            1
        );
    }

    #[test]
    fn inbox_relays_are_standard_public_outbound_state() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        let messaging = MessagingService::new(secrets);
        messaging
            .configure_inbox(
                &mut store,
                persona.id,
                vec!["wss://inbox.example".to_owned()],
                &["wss://discovery.example".to_owned()],
                20,
            )
            .unwrap();

        assert_eq!(
            store.state().inbox_relays.get(&persona.id).unwrap(),
            &["wss://inbox.example"]
        );
        let outbound = store.state().outbound.values().next().unwrap();
        assert_eq!(outbound.relays, vec!["wss://discovery.example"]);
        assert!(outbound.event_json.contains("\"kind\":10050"));
    }

    #[test]
    fn norms_are_discussable_propositions_without_special_enforcement() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        let service = DiscussionService::new(secrets);
        let norm = service
            .create_norm(
                &mut store,
                CreateNorm {
                    persona_id: persona.id,
                    statement: "Explain disagreements without harassment".to_owned(),
                    community: CommunityKey::parse("science").unwrap(),
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 20,
                },
            )
            .unwrap();
        let explanation = service
            .create_comment(
                &mut store,
                CreateComment {
                    persona_id: persona.id,
                    parent_anchor: norm.anchor.clone(),
                    body: "This keeps disagreement legible.".to_owned(),
                    relays: vec!["wss://relay.example".to_owned()],
                    recorded_at: 21,
                },
            )
            .unwrap();

        assert_eq!(norm.kind, ObjectKind::Norm);
        assert_eq!(explanation.root.as_ref(), Some(&norm.anchor));
        assert_eq!(store.state().outbound.len(), 5);
    }

    #[tokio::test]
    async fn accepted_relay_pairs_are_not_republished() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let persona = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        DiscussionService::new(secrets)
            .create_post(
                &mut store,
                CreatePost {
                    persona_id: persona.id,
                    title: "Fungal networks".to_owned(),
                    link_url: None,
                    body: "Still worth discussing".to_owned(),
                    communities: vec![CommunityKey::parse("science").unwrap()],
                    relays: vec![
                        "wss://one.example".to_owned(),
                        "wss://two.example".to_owned(),
                    ],
                    recorded_at: 20,
                },
            )
            .unwrap();
        let publisher = AcceptingPublisher::default();
        let report = SyncService::sync_pending(&mut store, &publisher, 30)
            .await
            .unwrap();
        assert_eq!(report.accepted, 4);
        assert_eq!(store.state().pending_delivery_count(), 0);
        assert_eq!(publisher.0.lock().unwrap().len(), 2);

        let second = SyncService::sync_pending(&mut store, &publisher, 40)
            .await
            .unwrap();
        assert_eq!(second, SyncReport::default());
        assert_eq!(publisher.0.lock().unwrap().len(), 2);
    }

    #[test]
    fn imported_persona_keeps_existing_identity_and_secret_out_of_the_log() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let keys = Keys::generate();
        let secret = keys.secret_key().to_bech32().unwrap();
        let expected = keys.public_key().to_bech32().unwrap();

        let persona = PersonaService::new(secrets.clone())
            .import(&mut store, "Existing Alice".to_owned(), &secret, 10)
            .unwrap();

        assert_eq!(persona.public_key.as_str(), expected);
        let credential = PersonaCredential::decode(&secrets.get(persona.id).unwrap()).unwrap();
        assert!(matches!(
            credential,
            PersonaCredential::Local { signing_secret } if signing_secret == secret
        ));
        let log = std::fs::read_to_string(root.path().join("events.jsonl")).unwrap();
        assert!(!log.contains(&secret));
    }

    #[test]
    fn encrypted_drafts_are_visible_only_to_their_own_persona() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let personas = PersonaService::new(secrets.clone());
        let alice = personas.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let bob = personas.create(&mut store, "Bob".to_owned(), 11).unwrap();
        let draft = DraftRecord {
            id: "draft-1".to_owned(),
            persona: alice.id,
            kind: hydra_domain::DraftKind::Post,
            title: Some("Private working title".to_owned()),
            link_url: None,
            body: "Private working body".to_owned(),
            communities: vec![CommunityKey::parse("science").unwrap()],
            parent: None,
            updated_at: 20,
            discarded: false,
        };
        DraftService::save(&secrets, &mut store, draft, 20).unwrap();

        assert!(
            private_state(&secrets, &store, alice.id)
                .unwrap()
                .drafts
                .contains_key("draft-1")
        );
        assert!(
            private_state(&secrets, &store, bob.id)
                .unwrap()
                .drafts
                .is_empty()
        );
        let log = std::fs::read_to_string(root.path().join("events.jsonl")).unwrap();
        assert!(!log.contains("Private working title"));
        assert!(!log.contains("Private working body"));
    }

    #[test]
    fn failed_public_commit_rolls_back_the_secret() {
        let secrets = MemorySecrets::default();
        let observer = secrets.clone();
        let result = PersonaService::new(secrets).create(&mut FailingSink, "Alice".to_owned(), 10);
        assert!(matches!(result, Err(AppError::Store(_))));
        assert!(observer.0.borrow().is_empty());
    }

    #[test]
    fn community_images_are_persona_scoped_signed_and_followable() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let personas = PersonaService::new(secrets.clone());
        let alice = personas.create(&mut store, "Alice".to_owned(), 10).unwrap();
        let bob = personas.create(&mut store, "Bob".to_owned(), 11).unwrap();
        let service = SocialService::new(secrets.clone());
        let initial_outbound = store.state().outbound.len();
        let image = flocking_core::CommunityImage {
            sha256: flocking_core::EventId::parse("1".repeat(64)).unwrap(),
            url: "https://images.example/science.png".to_owned(),
            mime_type: "image/png".to_owned(),
            width: 256,
            height: 256,
            alt: "A violet atom".to_owned(),
        };

        let record = service
            .set_community_appearance(
                &mut store,
                &SetCommunityAppearance {
                    persona_id: alice.id,
                    topic: CommunityKey::parse("science").unwrap(),
                    image: Some(image.clone()),
                    public: true,
                    relays: vec!["wss://relay.example".to_owned()],
                    changed_at: 20,
                },
            )
            .unwrap();
        assert!(record.appearance.event_id.is_some());
        assert_eq!(store.state().outbound.len(), initial_outbound + 1);
        let event_json = store
            .state()
            .outbound
            .values()
            .find(|event| event.event_json.contains("\"kind\":30821"))
            .unwrap()
            .event_json
            .clone();
        ImportService::receive_public(&mut store, &event_json, 21).unwrap();
        assert_eq!(store.state().community_appearances.len(), 1);
        assert_eq!(
            private_state(&secrets, &store, alice.id)
                .unwrap()
                .community_appearances["science"]
                .appearance
                .image,
            Some(image)
        );

        let profile = service
            .set_appearance_source(
                &mut store,
                &SetAppearanceSource {
                    persona_id: alice.id,
                    source: bob.public_key,
                    enabled: true,
                    complete: false,
                    changed_at: 21,
                },
            )
            .unwrap();
        assert_eq!(profile.config.appearance_sources.len(), 1);
        assert!(profile.appearance_complete_sources.is_empty());
    }

    #[test]
    fn community_colors_are_persona_scoped_signed_and_independent_from_images() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let secrets = MemorySecrets::default();
        let alice = PersonaService::new(secrets.clone())
            .create(&mut store, "Alice".to_owned(), 10)
            .unwrap();
        let scheme = CommunityColorScheme {
            light_base: "#b9d3eb".to_owned(),
            light_accent: "#326a9d".to_owned(),
            dark_base: "#182634".to_owned(),
            dark_accent: "#82b9e7".to_owned(),
        };

        let record = SocialService::new(secrets.clone())
            .set_community_color_scheme(
                &mut store,
                &SetCommunityColorScheme {
                    persona_id: alice.id,
                    topic: CommunityKey::parse("science").unwrap(),
                    scheme: Some(scheme.clone()),
                    public: true,
                    relays: vec!["wss://relay.example".to_owned()],
                    changed_at: 20,
                },
            )
            .unwrap();

        assert!(record.choice.event_id.is_some());
        assert_eq!(record.choice.scheme, Some(scheme.clone()));
        assert!(store.state().community_appearances.is_empty());
        let event_json = store
            .state()
            .outbound
            .values()
            .find(|event| event.event_json.contains("\"kind\":30802"))
            .unwrap()
            .event_json
            .clone();
        ImportService::receive_public(&mut store, &event_json, 21).unwrap();
        assert_eq!(store.state().community_color_choices.len(), 1);
        assert_eq!(
            private_state(&secrets, &store, alice.id)
                .unwrap()
                .community_color_choices["science"]
                .choice
                .scheme,
            Some(scheme)
        );
    }
}
