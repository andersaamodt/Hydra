#![forbid(unsafe_code)]
//! Nostr-native Hydra protocol construction.

use std::{collections::BTreeSet, future::Future, pin::Pin, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hydra_domain::{
    AnchorId, CommunityKey, ContentBody, MediaManifest, NostrPublicKey, ObjectHead, ObjectKind,
    OutboundEvent, Projection, PublicProjectionRecord, ReactionRecord, ReactionValue,
};
use nostr::signer::SignerBackend;
use nostr::{
    Event, EventBuilder, EventId, Filter, JsonUtil, Keys, Kind, NostrSigner, PublicKey, RelayUrl,
    SignerError, Tag, Timestamp, ToBech32, UnsignedEvent,
    nips::{
        nip01::Coordinate,
        nip02::Contact,
        nip09::EventDeletionRequest,
        nip19::Nip19Event,
        nip25::ReactionTarget,
        nip44,
        nip51::{Interests, MuteList},
        nip59::UnwrappedGift,
    },
};
use nostr_sdk::{Client, SyncOptions};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

pub use hydra_protocol::{OBJECT_HEAD_KIND, PROJECTION_RECORD_KIND, PROTOCOL_VERSION};

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("post title cannot be empty")]
    EmptyTitle,
    #[error("at least one Hydra community is required")]
    MissingCommunity,
    #[error("Nostr event construction failed: {0}")]
    Nostr(String),
    #[error("relay transport failed: {0}")]
    Relay(String),
}

/// A Nostr signer whose private key may live locally or behind a NIP-46
/// connection. The known public key makes synchronous protocol construction
/// deterministic while signing remains delegated to the underlying backend.
#[derive(Debug, Clone)]
pub struct HydraSigner {
    public_key: PublicKey,
    inner: Arc<dyn NostrSigner>,
    local: Option<Keys>,
}

impl HydraSigner {
    #[must_use]
    pub fn local(keys: Keys) -> Self {
        Self {
            public_key: keys.public_key(),
            inner: Arc::new(keys.clone()),
            local: Some(keys),
        }
    }

    #[must_use]
    pub fn remote(public_key: PublicKey, signer: Arc<dyn NostrSigner>) -> Self {
        Self {
            public_key,
            inner: signer,
            local: None,
        }
    }

    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        self.public_key
    }
}

impl NostrSigner for HydraSigner {
    fn backend(&self) -> SignerBackend<'_> {
        self.inner.backend()
    }

    fn get_public_key(&self) -> nostr::util::BoxedFuture<'_, Result<PublicKey, SignerError>> {
        let public_key = self.public_key;
        Box::pin(async move { Ok(public_key) })
    }

    fn sign_event(
        &self,
        unsigned: UnsignedEvent,
    ) -> nostr::util::BoxedFuture<'_, Result<Event, SignerError>> {
        self.inner.sign_event(unsigned)
    }

    fn nip04_encrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> nostr::util::BoxedFuture<'a, Result<String, SignerError>> {
        self.inner.nip04_encrypt(public_key, content)
    }

    fn nip04_decrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> nostr::util::BoxedFuture<'a, Result<String, SignerError>> {
        self.inner.nip04_decrypt(public_key, content)
    }

    fn nip44_encrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> nostr::util::BoxedFuture<'a, Result<String, SignerError>> {
        self.inner.nip44_encrypt(public_key, content)
    }

    fn nip44_decrypt<'a>(
        &'a self,
        public_key: &'a PublicKey,
        payload: &'a str,
    ) -> nostr::util::BoxedFuture<'a, Result<String, SignerError>> {
        self.inner.nip44_decrypt(public_key, payload)
    }
}

/// Synchronous construction port used by Hydra's local-first transaction
/// services. Remote implementations may bridge to their asynchronous signer.
pub trait EventSigner {
    fn public_key(&self) -> PublicKey;

    /// Signs the fully constructed event with the configured backend.
    ///
    /// # Errors
    ///
    /// Returns an error when local signing fails or a delegated signer rejects,
    /// times out, or returns an invalid response.
    fn sign(&self, builder: EventBuilder) -> Result<Event, ProtocolError>;
}

impl EventSigner for Keys {
    fn public_key(&self) -> PublicKey {
        self.public_key()
    }

    fn sign(&self, builder: EventBuilder) -> Result<Event, ProtocolError> {
        builder.sign_with_keys(self).map_err(nostr_error)
    }
}

impl EventSigner for HydraSigner {
    fn public_key(&self) -> PublicKey {
        self.public_key
    }

    fn sign(&self, builder: EventBuilder) -> Result<Event, ProtocolError> {
        if let Some(keys) = &self.local {
            return builder.sign_with_keys(keys).map_err(nostr_error);
        }
        let future = builder.sign(self);
        let signed = match tokio::runtime::Handle::try_current() {
            Ok(handle) => tokio::task::block_in_place(|| handle.block_on(future)),
            Err(_) => tokio::runtime::Runtime::new()
                .map_err(|error| ProtocolError::Nostr(error.to_string()))?
                .block_on(future),
        };
        signed.map_err(nostr_error)
    }
}

trait HydraEventBuilderExt {
    fn sign_with(self, signer: &impl EventSigner) -> Result<Event, ProtocolError>;
}

impl HydraEventBuilderExt for EventBuilder {
    fn sign_with(self, signer: &impl EventSigner) -> Result<Event, ProtocolError> {
        signer.sign(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayDelivery {
    pub accepted: Vec<String>,
    pub rejected: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayProbe {
    pub configured: usize,
    pub connected: usize,
}

/// Connects to configured relays and reports actual websocket readiness.
///
/// # Errors
///
/// Returns an error for invalid relay URLs. Individual unavailable relays are
/// reported through the connected count rather than failing the whole probe.
pub async fn probe_relays(
    relays: &[String],
    timeout: Duration,
) -> Result<RelayProbe, ProtocolError> {
    let client = Client::builder().build();
    for relay in relays {
        client
            .add_relay(relay)
            .await
            .map_err(|error| ProtocolError::Relay(error.to_string()))?;
    }
    client.connect().await;
    client.wait_for_connection(timeout).await;
    let connected = client
        .relays()
        .await
        .values()
        .filter(|relay| relay.status().to_string() == "Connected")
        .count();
    client.shutdown().await;
    Ok(RelayProbe {
        configured: relays.len(),
        connected,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateMessageWraps {
    pub rumor_id: String,
    pub recipient: Event,
    pub sender_copy: Event,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnwrappedDirectMessage {
    pub rumor_id: String,
    pub sender: NostrPublicKey,
    pub recipients: Vec<NostrPublicKey>,
    pub body: String,
    pub created_at: u64,
}

pub type PublishFuture<'a> =
    Pin<Box<dyn Future<Output = Result<RelayDelivery, ProtocolError>> + Send + 'a>>;

pub trait EventPublisher {
    /// Publishes one already-signed event to its selected relay destinations.
    ///
    /// # Errors
    ///
    /// Returns an error when the event is invalid or the relay client cannot run
    /// the publication operation at all. Per-relay failures are returned as data.
    fn publish<'a>(&'a self, event: &'a OutboundEvent) -> PublishFuture<'a>;
}

/// Builds a portable NIP-19/NIP-21 event URI with optional relay hints.
/// The event identifier remains recoverable if any particular gateway or
/// Hydra installation disappears.
///
/// # Errors
///
/// Returns an error when a relay hint is not a valid Nostr relay URL.
pub fn portable_event_uri(event: &Event, relays: &[String]) -> Result<String, ProtocolError> {
    let relays = relays
        .iter()
        .map(|relay| RelayUrl::parse(relay).map_err(nostr_error))
        .collect::<Result<Vec<_>, _>>()?;
    let entity = Nip19Event::from(event).relays(relays);
    let encoded = entity.to_bech32().map_err(nostr_error)?;
    Ok(format!("nostr:{encoded}"))
}

#[derive(Debug, Clone)]
pub struct SdkEventPublisher {
    client: Client,
}

impl SdkEventPublisher {
    /// Creates one relay client and registers the supplied relay URLs.
    ///
    /// # Errors
    ///
    /// Returns an error if any relay URL is invalid or cannot be registered.
    pub async fn new(relays: &[String]) -> Result<Self, ProtocolError> {
        let client = Client::builder().build();
        for relay in relays {
            client
                .add_relay(relay)
                .await
                .map_err(|error| ProtocolError::Relay(error.to_string()))?;
        }
        client.connect().await;
        Ok(Self { client })
    }
}

impl EventPublisher for SdkEventPublisher {
    fn publish<'a>(&'a self, event: &'a OutboundEvent) -> PublishFuture<'a> {
        Box::pin(async move {
            let signed = Event::from_json(&event.event_json)
                .map_err(|error| ProtocolError::Nostr(error.to_string()))?;
            let output = self
                .client
                .send_event_to(&event.relays, &signed)
                .await
                .map_err(|error| ProtocolError::Relay(error.to_string()))?;
            let mut accepted = output
                .success
                .into_iter()
                .map(|relay| relay.to_string())
                .collect::<Vec<_>>();
            let mut rejected = output
                .failed
                .into_iter()
                .map(|(relay, error)| (relay.to_string(), error))
                .collect::<Vec<_>>();
            accepted.sort();
            rejected.sort_by(|left, right| left.0.cmp(&right.0));
            Ok(RelayDelivery { accepted, rejected })
        })
    }
}

/// Fetches the bounded public discussion surface for selected ownerless topics.
/// This first attempts NIP-77 Negentropy down-sync and falls back to a bounded
/// ordinary request when the selected relay does not support reconciliation.
///
/// # Errors
///
/// Returns an error for invalid relay URLs or relay-pool failure.
pub async fn fetch_community_events(
    relays: &[String],
    communities: &[CommunityKey],
    since: Option<u64>,
) -> Result<Vec<Event>, ProtocolError> {
    if relays.is_empty() || communities.is_empty() {
        return Ok(Vec::new());
    }
    let client = Client::builder().build();
    for relay in relays {
        client
            .add_relay(relay)
            .await
            .map_err(|error| ProtocolError::Relay(error.to_string()))?;
    }
    client.connect().await;
    let mut filter = community_filter(communities);
    if let Some(since) = since {
        filter = filter.since(Timestamp::from(since));
    }
    let events = if client
        .sync(
            filter.clone(),
            &SyncOptions::default().initial_timeout(Duration::from_secs(3)),
        )
        .await
        .is_ok()
    {
        client
            .database()
            .query(filter.clone())
            .await
            .map_err(|error| ProtocolError::Relay(error.to_string()))?
            .into_iter()
            .collect()
    } else {
        client
            .fetch_events(filter, Duration::from_secs(8))
            .await
            .map_err(|error| ProtocolError::Relay(error.to_string()))?
            .into_iter()
            .collect()
    };
    client.disconnect().await;
    Ok(events)
}

/// Fetches a bounded general-purpose Nostr discussion feed.
///
/// Unlike community fetching, this deliberately applies no topic filter.
/// Callers keep the results transient until the user interacts with them.
///
/// # Errors
///
/// Returns an error for invalid relay URLs or relay-pool failure.
pub async fn fetch_open_events(
    relays: &[String],
    since: Option<u64>,
    limit: usize,
) -> Result<Vec<Event>, ProtocolError> {
    if relays.is_empty() {
        return Ok(Vec::new());
    }
    let client = Client::builder().build();
    for relay in relays {
        client
            .add_relay(relay)
            .await
            .map_err(|error| ProtocolError::Relay(error.to_string()))?;
    }
    client.connect().await;
    let mut filter = Filter::new()
        .kinds(open_content_kinds())
        .limit(limit.clamp(1, 100));
    if let Some(since) = since {
        filter = filter.since(Timestamp::from(since));
    }
    let events = client
        .fetch_events(filter, Duration::from_secs(8))
        .await
        .map_err(|error| ProtocolError::Relay(error.to_string()))?
        .into_iter()
        .collect();
    client.disconnect().await;
    Ok(events)
}

fn open_content_kinds() -> Vec<Kind> {
    vec![
        Kind::TextNote,
        Kind::Thread,
        Kind::LongFormTextNote,
        Kind::Custom(20),
        Kind::Custom(21),
        Kind::Custom(22),
    ]
}

/// Searches selected relays through the standard NIP-50 filter extension.
/// Results remain transient until the caller explicitly imports or interacts
/// with them.
///
/// # Errors
///
/// Returns an error for an empty query, invalid relay, or relay-pool failure.
pub async fn search_events(
    relays: &[String],
    query: &str,
    limit: usize,
) -> Result<Vec<Event>, ProtocolError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(ProtocolError::Nostr(
            "search query cannot be empty".to_owned(),
        ));
    }
    if relays.is_empty() {
        return Ok(Vec::new());
    }
    let client = Client::builder().build();
    for relay in relays {
        client
            .add_relay(relay)
            .await
            .map_err(|error| ProtocolError::Relay(error.to_string()))?;
    }
    client.connect().await;
    let events = client
        .fetch_events(
            Filter::new()
                .kinds(discussion_kinds())
                .search(query)
                .limit(limit.clamp(1, 200)),
            Duration::from_secs(8),
        )
        .await
        .map_err(|error| ProtocolError::Relay(error.to_string()))?
        .into_iter()
        .collect();
    client.disconnect().await;
    Ok(events)
}

/// Fetches the bounded NIP-17 inbox for one persona using authenticated relay
/// access where the relay requires NIP-42.
///
/// # Errors
///
/// Returns an error for invalid relay URLs or relay-pool failure.
pub async fn fetch_gift_wraps<S>(
    relays: &[String],
    signer: &S,
    since: Option<u64>,
) -> Result<Vec<Event>, ProtocolError>
where
    S: NostrSigner + Clone + 'static,
{
    if relays.is_empty() {
        return Ok(Vec::new());
    }
    let public_key = signer.get_public_key().await.map_err(nostr_error)?;
    let client = Client::builder().signer(signer.clone()).build();
    for relay in relays {
        client
            .add_relay(relay)
            .await
            .map_err(|error| ProtocolError::Relay(error.to_string()))?;
    }
    client.connect().await;
    let mut filter = Filter::new()
        .kind(Kind::GiftWrap)
        .pubkey(public_key)
        .limit(500);
    if let Some(since) = since {
        filter = filter.since(Timestamp::from(since));
    }
    let events = client
        .fetch_events(filter, Duration::from_secs(8))
        .await
        .map_err(|error| ProtocolError::Relay(error.to_string()))?
        .into_iter()
        .collect();
    client.disconnect().await;
    Ok(events)
}

/// Discovers a recipient's latest standard NIP-17 inbox relay declaration.
///
/// # Errors
///
/// Returns an error for an invalid recipient, relay, or relay-pool failure.
pub async fn discover_inbox_relays(
    discovery_relays: &[String],
    recipient: &NostrPublicKey,
) -> Result<Vec<String>, ProtocolError> {
    if discovery_relays.is_empty() {
        return Ok(Vec::new());
    }
    let recipient = PublicKey::parse(recipient.as_str()).map_err(nostr_error)?;
    let client = Client::builder().build();
    for relay in discovery_relays {
        client
            .add_relay(relay)
            .await
            .map_err(|error| ProtocolError::Relay(error.to_string()))?;
    }
    client.connect().await;
    let events = client
        .fetch_events(
            Filter::new()
                .author(recipient)
                .kind(Kind::InboxRelays)
                .limit(1),
            Duration::from_secs(8),
        )
        .await
        .map_err(|error| ProtocolError::Relay(error.to_string()))?;
    client.disconnect().await;
    let relays = events
        .into_iter()
        .max_by_key(|event| event.created_at)
        .map(|event| {
            event
                .tags
                .iter()
                .filter(|tag| tag.as_slice().first().map(String::as_str) == Some("relay"))
                .filter_map(|tag| tag.content())
                .filter(|relay| RelayUrl::parse(relay).is_ok())
                .map(str::to_owned)
                .take(3)
                .collect()
        })
        .unwrap_or_default();
    Ok(relays)
}

/// Builds the standard NIP-17 preferred inbox relay declaration.
///
/// # Errors
///
/// Returns an error for an empty/invalid relay list or signing failure.
pub fn inbox_relays(
    signer: &impl EventSigner,
    relays: &[String],
    created_at: u64,
) -> Result<Event, ProtocolError> {
    if relays.is_empty() {
        return Err(ProtocolError::Relay(
            "at least one inbox relay is required".to_owned(),
        ));
    }
    let tags = relays
        .iter()
        .map(|relay| {
            RelayUrl::parse(relay).map_err(nostr_error)?;
            tag(["relay", relay])
        })
        .collect::<Result<Vec<_>, _>>()?;
    EventBuilder::new(Kind::InboxRelays, "")
        .tags(tags)
        .custom_created_at(Timestamp::from(created_at))
        .sign_with(signer)
}

/// Publishes the persona's standard NIP-01 profile metadata.
///
/// # Errors
///
/// Returns an error for an empty/oversized name, serialization, or signing.
pub fn profile_metadata(
    signer: &impl EventSigner,
    display_name: &str,
    changed_at: u64,
) -> Result<Event, ProtocolError> {
    let display_name = display_name.trim();
    if display_name.is_empty() || display_name.len() > 100 {
        return Err(ProtocolError::Nostr(
            "profile display name must contain 1-100 characters".to_owned(),
        ));
    }
    let content = serde_json::json!({
        "name": display_name,
        "display_name": display_name,
    })
    .to_string();
    EventBuilder::new(Kind::Metadata, content)
        .custom_created_at(Timestamp::from(changed_at))
        .sign_with(signer)
}

/// Publishes Hydra's NIP-39-compatible Reddit identity claim. Reddit is an
/// extensible provider convention: the proof is a verified public artifact URL.
///
/// # Errors
///
/// Returns an error for an invalid username/artifact or signing failure.
pub fn reddit_identity_proof(
    signer: &impl EventSigner,
    proof: &hydra_domain::RedditIdentityProof,
) -> Result<Event, ProtocolError> {
    proof
        .validate()
        .map_err(|error| ProtocolError::Nostr(error.to_string()))?;
    EventBuilder::new(Kind::Custom(10_011), "")
        .tag(tag([
            "i",
            &format!("reddit:{}", proof.username.to_lowercase()),
            &proof.artifact_url,
        ])?)
        .custom_created_at(Timestamp::from(proof.published_at))
        .sign_with(signer)
}

/// Publishes standard NIP-65 read/write relay preferences for one persona.
/// Relays serving both directions use an unmarked `r` tag as specified by
/// NIP-65.
///
/// # Errors
///
/// Returns an error for an empty set, malformed relay URL, or signing failure.
pub fn persona_relay_list(
    signer: &impl EventSigner,
    read: &[String],
    write: &[String],
    changed_at: u64,
) -> Result<Event, ProtocolError> {
    if read.is_empty() || write.is_empty() {
        return Err(ProtocolError::Nostr(
            "NIP-65 requires at least one read and write relay".to_owned(),
        ));
    }
    let read = read.iter().collect::<BTreeSet<_>>();
    let write = write.iter().collect::<BTreeSet<_>>();
    let mut relays = read.union(&write).copied().collect::<Vec<_>>();
    relays.sort();
    let tags = relays
        .into_iter()
        .map(|relay| {
            let parsed = RelayUrl::parse(relay).map_err(nostr_error)?;
            match (read.contains(relay), write.contains(relay)) {
                (true, true) => tag(["r", parsed.as_str()]),
                (true, false) => tag(["r", parsed.as_str(), "read"]),
                (false, true) => tag(["r", parsed.as_str(), "write"]),
                (false, false) => unreachable!("union contains the relay"),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    EventBuilder::new(Kind::RelayList, "")
        .tags(tags)
        .custom_created_at(Timestamp::from(changed_at))
        .sign_with(signer)
}

#[must_use]
pub fn community_filter(communities: &[CommunityKey]) -> Filter {
    Filter::new()
        .kinds(discussion_kinds())
        .hashtags(communities.iter().map(CommunityKey::as_str))
        .limit(500)
}

fn discussion_kinds() -> Vec<Kind> {
    vec![
        Kind::TextNote,
        Kind::Thread,
        Kind::Comment,
        Kind::LongFormTextNote,
        Kind::Custom(20),
        Kind::Custom(21),
        Kind::Custom(22),
        Kind::Repost,
        Kind::GenericRepost,
        Kind::Custom(OBJECT_HEAD_KIND),
        Kind::Custom(PROJECTION_RECORD_KIND),
        Kind::Reaction,
        Kind::Label,
    ]
}

#[derive(Debug, Clone, Copy)]
pub struct EventReference {
    pub id: EventId,
    pub kind: Kind,
    pub author: PublicKey,
}

#[derive(Debug, Clone, Copy)]
pub struct CommentScope {
    pub root: EventReference,
    pub parent: EventReference,
}

#[derive(Debug, Clone)]
pub struct ExternalCommentScope {
    pub root: hydra_domain::ExternalId,
    pub parent: hydra_domain::ExternalId,
}

/// Builds a standard NIP-7D thread anchor.
///
/// # Errors
///
/// Returns an error for missing title/community or invalid Nostr tags/signing.
pub fn post_anchor(
    signer: &impl EventSigner,
    title: &str,
    body: &str,
    communities: &[CommunityKey],
    created_at: u64,
) -> Result<Event, ProtocolError> {
    validate_title(title)?;
    require_communities(communities)?;
    let mut tags = vec![tag(["title", title])?];
    tags.extend(community_tags(communities)?);
    EventBuilder::new(Kind::Thread, body)
        .tags(tags)
        .custom_created_at(Timestamp::from(created_at))
        .sign_with(signer)
}

/// Builds a NIP-7D post anchor for content the signer authored on an external
/// service and supplied through that service's account-data export.
///
/// NIP-73 and NIP-48 tags retain the original public URL without attributing
/// anyone else's content to the signer.
///
/// # Errors
///
/// Returns an error for a missing title/community, an invalid source, or
/// invalid Nostr tags/signing.
pub fn imported_post_anchor(
    signer: &impl EventSigner,
    title: &str,
    body: &str,
    communities: &[CommunityKey],
    source: &hydra_domain::ExternalId,
    created_at: u64,
) -> Result<Event, ProtocolError> {
    validate_title(title)?;
    require_communities(communities)?;
    source.validate().map_err(domain_protocol_error)?;
    let mut tags = vec![
        tag(["title", title])?,
        tag(["i", &source.canonical])?,
        tag(["proxy", &source.canonical, "web"])?,
    ];
    tags.extend(community_tags(communities)?);
    EventBuilder::new(Kind::Thread, body)
        .tags(tags)
        .custom_created_at(Timestamp::from(created_at))
        .sign_with(signer)
}

/// Builds the addressable editable head for a post or comment anchor.
///
/// # Errors
///
/// Returns an error for missing community or invalid Nostr tags/signing.
pub fn object_head(
    signer: &impl EventSigner,
    anchor: EventReference,
    comment_scope: Option<CommentScope>,
    title: Option<&str>,
    body: &str,
    communities: &[CommunityKey],
    edited_at: u64,
) -> Result<Event, ProtocolError> {
    require_communities(communities)?;
    let mut tags = vec![
        tag(["d", &format!("hydra:head:{}", anchor.id.to_hex())])?,
        tag(["e", &anchor.id.to_hex()])?,
        tag(["k", &u16::from(anchor.kind).to_string()])?,
        tag(["L", "hydra"])?,
        tag(["l", "object-head", "hydra"])?,
        tag(["version", PROTOCOL_VERSION])?,
    ];
    append_comment_scope_tags(&mut tags, comment_scope)?;
    if let Some(title) = title {
        validate_title(title)?;
        tags.push(tag(["title", title])?);
    }
    tags.extend(community_tags(communities)?);
    EventBuilder::new(Kind::Custom(OBJECT_HEAD_KIND), body)
        .tags(tags)
        .custom_created_at(Timestamp::from(edited_at))
        .sign_with(signer)
}

/// Builds an edited Hydra content head with standard NIP-92 inline metadata for
/// media URLs that also occur in the content body.
///
/// # Errors
///
/// Returns an error for missing communities, absent blob URLs, invalid tags,
/// or signing failure.
#[allow(
    clippy::too_many_arguments,
    reason = "the editable object wire contract keeps every signed field explicit"
)]
pub fn object_head_with_media(
    signer: &impl EventSigner,
    anchor: EventReference,
    comment_scope: Option<CommentScope>,
    title: Option<&str>,
    body: &str,
    communities: &[CommunityKey],
    media: &[MediaManifest],
    edited_at: u64,
) -> Result<Event, ProtocolError> {
    require_communities(communities)?;
    let mut tags = vec![
        tag(["d", &format!("hydra:head:{}", anchor.id.to_hex())])?,
        tag(["e", &anchor.id.to_hex()])?,
        tag(["k", &u16::from(anchor.kind).to_string()])?,
        tag(["L", "hydra"])?,
        tag(["l", "object-head", "hydra"])?,
        tag(["version", PROTOCOL_VERSION])?,
    ];
    append_comment_scope_tags(&mut tags, comment_scope)?;
    if let Some(title) = title {
        validate_title(title)?;
        tags.push(tag(["title", title])?);
    }
    tags.extend(community_tags(communities)?);
    for manifest in media {
        let Some(primary) = manifest.blob_urls.first() else {
            return Err(ProtocolError::Nostr(
                "NIP-92 attachment requires a public blob URL".to_owned(),
            ));
        };
        if !body.contains(primary) {
            return Err(ProtocolError::Nostr(
                "NIP-92 attachment URL must occur in event content".to_owned(),
            ));
        }
        let mut parts = vec![
            "imeta".to_owned(),
            format!("url {primary}"),
            format!("m {}", manifest.mime_type.to_lowercase()),
            format!("x {}", manifest.sha256),
            format!("size {}", manifest.size),
        ];
        if let Some(dimensions) = &manifest.dimensions {
            parts.push(format!("dim {dimensions}"));
        }
        if let Some(duration) = manifest.duration_seconds {
            parts.push(format!("duration {duration}"));
        }
        parts.extend(
            manifest
                .blob_urls
                .iter()
                .skip(1)
                .map(|url| format!("fallback {url}")),
        );
        tags.push(Tag::parse(parts).map_err(nostr_error)?);
    }
    EventBuilder::new(Kind::Custom(OBJECT_HEAD_KIND), body)
        .tags(tags)
        .custom_created_at(Timestamp::from(edited_at))
        .sign_with(signer)
}

/// Builds a NIP-22 nested comment anchored to the immutable root and parent.
///
/// # Errors
///
/// Returns an error for missing community or invalid Nostr tags/signing.
pub fn comment_anchor(
    signer: &impl EventSigner,
    body: &str,
    scope: CommentScope,
    communities: &[CommunityKey],
    created_at: u64,
) -> Result<Event, ProtocolError> {
    require_communities(communities)?;
    let mut tags = vec![
        tag(["E", &scope.root.id.to_hex()])?,
        tag(["K", &u16::from(scope.root.kind).to_string()])?,
        tag(["P", &scope.root.author.to_hex()])?,
        tag(["e", &scope.parent.id.to_hex()])?,
        tag(["k", &u16::from(scope.parent.kind).to_string()])?,
        tag(["p", &scope.parent.author.to_hex()])?,
    ];
    tags.extend(community_tags(communities)?);
    EventBuilder::new(Kind::Comment, body)
        .tags(tags)
        .custom_created_at(Timestamp::from(created_at))
        .sign_with(signer)
}

/// Builds a standard NIP-22 comment whose stable root and parent are external
/// web objects such as Reddit permalinks.
///
/// # Errors
///
/// Returns an error for missing communities, malformed tags, or signing.
pub fn external_comment_anchor(
    signer: &impl EventSigner,
    body: &str,
    scope: &ExternalCommentScope,
    source: Option<&hydra_domain::ExternalId>,
    communities: &[CommunityKey],
    created_at: u64,
) -> Result<Event, ProtocolError> {
    require_communities(communities)?;
    let mut tags = vec![
        tag(["I", &scope.root.canonical])?,
        tag(["K", "web"])?,
        tag(["i", &scope.parent.canonical])?,
        tag(["k", "web"])?,
    ];
    if let Some(source) = source {
        source.validate().map_err(domain_protocol_error)?;
        tags.push(tag(["proxy", &source.canonical, "web"])?);
    }
    tags.extend(community_tags(communities)?);
    EventBuilder::new(Kind::Comment, body)
        .tags(tags)
        .custom_created_at(Timestamp::from(created_at))
        .sign_with(signer)
}

/// Builds an editable content head while retaining an external NIP-22 root
/// and parent for indexing and merged-tree reconstruction.
///
/// # Errors
///
/// Returns an error for missing communities, malformed tags, or signing.
pub fn external_comment_head(
    signer: &impl EventSigner,
    anchor: EventReference,
    body: &str,
    scope: &ExternalCommentScope,
    source: Option<&hydra_domain::ExternalId>,
    communities: &[CommunityKey],
    edited_at: u64,
) -> Result<Event, ProtocolError> {
    require_communities(communities)?;
    let mut tags = vec![
        tag(["d", &format!("hydra:head:{}", anchor.id.to_hex())])?,
        tag(["e", &anchor.id.to_hex()])?,
        tag(["k", &u16::from(anchor.kind).to_string()])?,
        tag(["I", &scope.root.canonical])?,
        tag(["K", "web"])?,
        tag(["i", &scope.parent.canonical])?,
        tag(["L", "hydra"])?,
        tag(["l", "object-head", "hydra"])?,
        tag(["version", PROTOCOL_VERSION])?,
    ];
    if let Some(source) = source {
        source.validate().map_err(domain_protocol_error)?;
        tags.push(tag(["proxy", &source.canonical, "web"])?);
    }
    tags.extend(community_tags(communities)?);
    EventBuilder::new(Kind::Custom(OBJECT_HEAD_KIND), body)
        .tags(tags)
        .custom_created_at(Timestamp::from(edited_at))
        .sign_with(signer)
}

/// Builds a NIP-22 comment with an external root and an immediate Nostr parent.
///
/// # Errors
///
/// Returns an error for missing communities, malformed tags, or signing.
pub fn external_root_comment_anchor(
    signer: &impl EventSigner,
    body: &str,
    root: &hydra_domain::ExternalId,
    parent: EventReference,
    communities: &[CommunityKey],
    created_at: u64,
) -> Result<Event, ProtocolError> {
    require_communities(communities)?;
    let mut tags = vec![
        tag(["I", &root.canonical])?,
        tag(["K", "web"])?,
        tag(["e", &parent.id.to_hex()])?,
        tag(["k", &u16::from(parent.kind).to_string()])?,
        tag(["p", &parent.author.to_hex()])?,
    ];
    tags.extend(community_tags(communities)?);
    EventBuilder::new(Kind::Comment, body)
        .tags(tags)
        .custom_created_at(Timestamp::from(created_at))
        .sign_with(signer)
}

/// Builds the editable head for a comment with an external root and Nostr
/// immediate parent.
///
/// # Errors
///
/// Returns an error for missing communities, malformed tags, or signing.
pub fn external_root_comment_head(
    signer: &impl EventSigner,
    anchor: EventReference,
    body: &str,
    root: &hydra_domain::ExternalId,
    parent: EventReference,
    communities: &[CommunityKey],
    edited_at: u64,
) -> Result<Event, ProtocolError> {
    require_communities(communities)?;
    let mut tags = vec![
        tag(["d", &format!("hydra:head:{}", anchor.id.to_hex())])?,
        tag(["e", &anchor.id.to_hex()])?,
        tag(["k", &u16::from(anchor.kind).to_string()])?,
        tag(["I", &root.canonical])?,
        tag(["K", "web"])?,
        tag([
            "parent",
            &parent.id.to_hex(),
            &u16::from(parent.kind).to_string(),
            &parent.author.to_hex(),
        ])?,
        tag(["L", "hydra"])?,
        tag(["l", "object-head", "hydra"])?,
        tag(["version", PROTOCOL_VERSION])?,
    ];
    tags.extend(community_tags(communities)?);
    EventBuilder::new(Kind::Custom(OBJECT_HEAD_KIND), body)
        .tags(tags)
        .custom_created_at(Timestamp::from(edited_at))
        .sign_with(signer)
}

/// Builds a standard NIP-25 reaction targeting an immutable discussion anchor.
///
/// # Errors
///
/// Returns an error when signing fails.
pub fn reaction(
    signer: &impl EventSigner,
    target: EventReference,
    value: &str,
    created_at: u64,
) -> Result<Event, ProtocolError> {
    EventBuilder::reaction(
        ReactionTarget {
            event_id: target.id,
            public_key: target.author,
            coordinate: None,
            kind: Some(target.kind),
            relay_hint: None,
        },
        value,
    )
    .custom_created_at(Timestamp::from(created_at))
    .sign_with(signer)
}

/// Builds a standard NIP-25 kind 17 reaction to normalized external web
/// content.
///
/// # Errors
///
/// Returns an error for malformed tags or signing.
pub fn external_reaction(
    signer: &impl EventSigner,
    target: &hydra_domain::ExternalId,
    value: &str,
    created_at: u64,
) -> Result<Event, ProtocolError> {
    EventBuilder::new(Kind::Custom(17), value)
        .tags(vec![tag(["k", "web"])?, tag(["i", &target.canonical])?])
        .custom_created_at(Timestamp::from(created_at))
        .sign_with(signer)
}

/// Builds a standard NIP-18 repost and adds the selected Hydra topic tags.
///
/// The original signed event remains the authored content; the curator signs
/// only the repost and its categorization.
///
/// # Errors
///
/// Returns an error for an invalid source event, topic, or signature.
pub fn curation_repost(
    signer: &impl EventSigner,
    source: &Event,
    communities: &[CommunityKey],
    created_at: u64,
) -> Result<Event, ProtocolError> {
    source.verify().map_err(nostr_error)?;
    require_communities(communities)?;
    EventBuilder::repost(source, None)
        .tags(community_tags(communities)?)
        .custom_created_at(Timestamp::from(created_at))
        .sign_with(signer)
}

/// Builds the current addressable synchronization record for one projection.
///
/// # Errors
///
/// Returns an error for serialization, invalid tags, or signing.
pub fn projection_record(
    signer: &impl EventSigner,
    projection: &Projection,
    recorded_at: u64,
) -> Result<Event, ProtocolError> {
    let external = projection.external_id.as_ref().ok_or_else(|| {
        ProtocolError::Nostr("a public projection record requires an external object".to_owned())
    })?;
    let public_state = public_projection_state(projection.state).ok_or_else(|| {
        ProtocolError::Nostr("temporary projection state is private local data".to_owned())
    })?;
    let projection_type = match external.canonical.get(..3) {
        Some("t3_") => "post",
        Some("t1_") => "comment",
        _ => {
            return Err(ProtocolError::Nostr(
                "a public projection record requires a Reddit fullname".to_owned(),
            ));
        }
    };
    if !matches!(
        (projection_type, external.canonical.get(..3)),
        ("post", Some("t3_")) | ("comment", Some("t1_"))
    ) {
        return Err(ProtocolError::Nostr(
            "Reddit projection type does not match its fullname".to_owned(),
        ));
    }
    let external_url = projection.external_url.as_deref().ok_or_else(|| {
        ProtocolError::Nostr("a public projection record requires a Reddit URL".to_owned())
    })?;
    let (target_subreddit, url_fullname) = reddit_projection_target(external_url, projection_type)?;
    if url_fullname != external.canonical {
        return Err(ProtocolError::Nostr(
            "Reddit projection URL does not match its fullname".to_owned(),
        ));
    }
    let address_material = format!(
        "{}|{}|{}",
        projection.anchor.as_str(),
        external.system,
        external.canonical
    );
    let address = content_hash(&address_material);
    let mut tags = vec![
        tag(["d", &format!("hydra:projection:{address}")])?,
        tag(["e", projection.anchor.as_str()])?,
        tag(["L", "hydra"])?,
        tag(["l", "projection-record", "hydra"])?,
        tag(["version", PROTOCOL_VERSION])?,
        tag(["external", &external.system, &external.canonical])?,
    ];
    if let Some(url) = &projection.external_url {
        tags.push(tag(["i", url])?);
        tags.push(tag(["k", "web"])?);
    }
    tags.push(tag(["t", &target_subreddit])?);
    let public = serde_json::json!({
        "schema_version": PROTOCOL_VERSION,
        "anchor": projection.anchor,
        "external_id": external,
        "external_url": projection.external_url,
        "reddit_fullname": external.canonical,
        "target_subreddit": target_subreddit,
        "projection_type": projection_type,
        "state": public_state,
        "current_head": projection.last_synced_head,
    });
    EventBuilder::new(
        Kind::Custom(PROJECTION_RECORD_KIND),
        serde_json::to_string(&public).map_err(nostr_error)?,
    )
    .tags(tags)
    .custom_created_at(Timestamp::from(recorded_at))
    .sign_with(signer)
}

fn reddit_projection_target(
    value: &str,
    projection_type: &str,
) -> Result<(String, String), ProtocolError> {
    let url = Url::parse(value).map_err(nostr_error)?;
    if url.scheme() != "https"
        || !matches!(url.host_str(), Some("www.reddit.com" | "old.reddit.com"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ProtocolError::Nostr(
            "projection URL must be an HTTPS Reddit permalink".to_owned(),
        ));
    }
    let segments = url
        .path_segments()
        .ok_or_else(|| ProtocolError::Nostr("Reddit permalink has no path".to_owned()))?
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.first().copied() != Some("r")
        || segments.get(2).copied() != Some("comments")
        || segments.get(3).is_none_or(|value| value.is_empty())
    {
        return Err(ProtocolError::Nostr(
            "Reddit projection URL is not a canonical permalink".to_owned(),
        ));
    }
    let community = CommunityKey::parse(segments[1]).map_err(domain_protocol_error)?;
    let fullname = match projection_type {
        "post" => format!("t3_{}", segments[3]),
        "comment" => format!(
            "t1_{}",
            segments
                .get(5)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ProtocolError::Nostr(
                        "Reddit comment projection URL has no comment identifier".to_owned(),
                    )
                })?
        ),
        _ => {
            return Err(ProtocolError::Nostr(
                "unknown Reddit projection type".to_owned(),
            ));
        }
    };
    Ok((community.as_str().to_owned(), fullname))
}

/// Maps local projection state to the intentionally small public wire state.
/// Retry, rejection, divergence, timestamps, payloads, and errors remain local.
#[must_use]
pub const fn public_projection_state(state: hydra_domain::ProjectionState) -> Option<&'static str> {
    use hydra_domain::ProjectionState;
    match state {
        ProjectionState::Live | ProjectionState::Diverged | ProjectionState::Synchronizing => {
            Some("live")
        }
        ProjectionState::Locked => Some("locked"),
        ProjectionState::Removed => Some("removed"),
        ProjectionState::Deleted => Some("deleted"),
        ProjectionState::Withdrawn => Some("withdrawn"),
        ProjectionState::NotRequested
        | ProjectionState::Queued
        | ProjectionState::Submitting
        | ProjectionState::Rejected
        | ProjectionState::Failed
        | ProjectionState::Abandoned => None,
    }
}

#[must_use]
pub fn content_hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

/// Creates one BUD-11 upload authorization token scoped to both the exact blob
/// hash and Blossom server hostname.
///
/// # Errors
///
/// Returns an error for invalid tag data or signing failure.
pub fn blossom_upload_authorization(
    signer: &impl EventSigner,
    server_host: &str,
    sha256: &str,
    created_at: u64,
) -> Result<String, ProtocolError> {
    let expiration = created_at.saturating_add(5 * 60);
    let event = EventBuilder::new(Kind::Custom(24_242), "Upload Blob")
        .tags([
            tag(["t", "upload"])?,
            tag(["expiration", &expiration.to_string()])?,
            tag(["x", sha256])?,
            tag(["server", &server_host.to_lowercase()])?,
        ])
        .custom_created_at(Timestamp::from(created_at))
        .sign_with(signer)?;
    Ok(format!(
        "Nostr {}",
        URL_SAFE_NO_PAD.encode(event.as_json().as_bytes())
    ))
}

/// Publishes standard NIP-94 metadata for an already preserved public blob.
///
/// # Errors
///
/// Returns an error for missing public URLs, invalid metadata, or signing
/// failure.
pub fn file_metadata(
    signer: &impl EventSigner,
    manifest: &MediaManifest,
    description: &str,
    created_at: u64,
) -> Result<Event, ProtocolError> {
    if manifest.blob_urls.is_empty() {
        return Err(ProtocolError::Nostr(
            "NIP-94 metadata requires a public blob URL".to_owned(),
        ));
    }
    let mut tags = vec![
        tag(["url", manifest.blob_urls[0].as_str()])?,
        tag(["m", manifest.mime_type.to_lowercase().as_str()])?,
        tag(["x", manifest.sha256.as_str()])?,
        tag(["ox", manifest.sha256.as_str()])?,
        tag(["size", &manifest.size.to_string()])?,
    ];
    if let Some(dimensions) = &manifest.dimensions {
        tags.push(tag(["dim", dimensions])?);
    }
    if let Some(duration) = manifest.duration_seconds {
        tags.push(tag(["duration", &duration.to_string()])?);
    }
    tags.extend(
        manifest
            .blob_urls
            .iter()
            .skip(1)
            .map(|url| tag(["fallback", url.as_str()]))
            .collect::<Result<Vec<_>, _>>()?,
    );
    EventBuilder::new(Kind::Custom(1063), description)
        .tags(tags)
        .custom_created_at(Timestamp::from(created_at))
        .sign_with(signer)
}

/// Validates and converts one public Nostr discussion event into a Hydra head.
/// Addressable head events update an already known immutable anchor; unrelated
/// standard Nostr events return `None` without becoming Hydra objects.
///
/// # Errors
///
/// Returns an error for invalid signatures, malformed Hydra discussion tags,
/// or an addressable head that attempts to change authorship.
pub fn received_object_head(
    event: &Event,
    current: Option<&ObjectHead>,
) -> Result<Option<ObjectHead>, ProtocolError> {
    event.verify().map_err(nostr_error)?;
    if event.kind == Kind::Thread {
        return native_thread_head(event).map(Some);
    }
    if event.kind == Kind::Comment {
        return native_comment_head(event).map(Some);
    }
    if matches!(
        event.kind,
        Kind::TextNote | Kind::LongFormTextNote | Kind::Custom(20..=22)
    ) {
        return open_nostr_head(event).map(Some);
    }
    if event.kind == Kind::Custom(OBJECT_HEAD_KIND) {
        return editable_received_head(event, current).map(Some);
    }
    Ok(None)
}

/// Verifies and decodes one public Hydra-to-Reddit projection record.
///
/// Local retry state, credentials, rendered payloads, and errors are absent;
/// this record contains only portable provenance and public source state.
///
/// # Errors
///
/// Returns an error for invalid signatures, contradictory tags or content,
/// unsupported versions, unsafe Reddit URLs, or invalid public state.
#[allow(
    clippy::too_many_lines,
    reason = "one linear verifier makes all cross-field wire invariants auditable together"
)]
pub fn received_projection_record(
    event: &Event,
) -> Result<Option<PublicProjectionRecord>, ProtocolError> {
    if event.kind != Kind::Custom(PROJECTION_RECORD_KIND) {
        return Ok(None);
    }
    event.verify().map_err(nostr_error)?;
    let anchor = unique_tag_value(event, "e")?;
    let address = unique_tag_value(event, "d")?;
    let external_url = unique_tag_value(event, "i")?;
    let target_subreddit = unique_tag_value(event, "t")?;
    if unique_tag_value(event, "k")? != "web"
        || unique_tag_value(event, "L")? != "hydra"
        || unique_tag_value(event, "version")? != PROTOCOL_VERSION
        || !has_exact_tag(event, &["l", "projection-record", "hydra"])
    {
        return Err(ProtocolError::Nostr(
            "projection record metadata is invalid".to_owned(),
        ));
    }
    let external_tags = event
        .tags
        .iter()
        .filter(|tag| {
            tag.as_slice()
                .first()
                .is_some_and(|value| value == "external")
        })
        .collect::<Vec<_>>();
    if external_tags.len() != 1 || external_tags[0].as_slice().len() != 3 {
        return Err(ProtocolError::Nostr(
            "projection record external tag is invalid".to_owned(),
        ));
    }
    let external_system = external_tags[0].as_slice()[1].as_str();
    let external_canonical = external_tags[0].as_slice()[2].as_str();
    let expected_address = format!(
        "hydra:projection:{}",
        content_hash(&format!("{anchor}|{external_system}|{external_canonical}"))
    );
    if address != expected_address {
        return Err(ProtocolError::Nostr(
            "projection record address is not deterministic".to_owned(),
        ));
    }
    let content: serde_json::Value = serde_json::from_str(&event.content).map_err(nostr_error)?;
    if content
        .get("schema_version")
        .and_then(|value| value.as_str())
        != Some(PROTOCOL_VERSION)
    {
        return Err(ProtocolError::Nostr(
            "projection record schema version is unsupported".to_owned(),
        ));
    }
    let content_anchor: AnchorId = serde_json::from_value(
        content
            .get("anchor")
            .cloned()
            .ok_or_else(|| ProtocolError::Nostr("projection anchor is missing".to_owned()))?,
    )
    .map_err(nostr_error)?;
    let external_id: hydra_domain::ExternalId = serde_json::from_value(
        content
            .get("external_id")
            .cloned()
            .ok_or_else(|| ProtocolError::Nostr("projection external id is missing".to_owned()))?,
    )
    .map_err(nostr_error)?;
    let content_url = json_string(&content, "external_url")?;
    let reddit_fullname = json_string(&content, "reddit_fullname")?;
    let content_subreddit = json_string(&content, "target_subreddit")?;
    let projection_type = json_string(&content, "projection_type")?;
    let state = json_string(&content, "state")?;
    let current_head = content
        .get("current_head")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    if content_anchor.as_str() != anchor
        || external_id.system != external_system
        || external_id.canonical != external_canonical
        || content_url != external_url
        || reddit_fullname != external_canonical
        || content_subreddit != target_subreddit
        || !matches!(
            (projection_type, reddit_fullname.get(..3)),
            ("post", Some("t3_")) | ("comment", Some("t1_"))
        )
    {
        return Err(ProtocolError::Nostr(
            "projection record content contradicts its tags".to_owned(),
        ));
    }
    let (url_subreddit, url_fullname) = reddit_projection_target(content_url, projection_type)?;
    if url_subreddit != content_subreddit || url_fullname != reddit_fullname {
        return Err(ProtocolError::Nostr(
            "projection record URL contradicts its Reddit identity".to_owned(),
        ));
    }
    let record = PublicProjectionRecord {
        source_event_id: event.id.to_hex(),
        address: address.to_owned(),
        author: event_author(event)?,
        anchor: content_anchor,
        external_id,
        external_url: content_url.to_owned(),
        reddit_fullname: reddit_fullname.to_owned(),
        target_subreddit: target_subreddit.to_owned(),
        projection_type: projection_type.to_owned(),
        state: state.to_owned(),
        current_head,
        recorded_at: event.created_at.as_secs(),
    };
    record.validate().map_err(domain_protocol_error)?;
    Ok(Some(record))
}

fn json_string<'a>(value: &'a serde_json::Value, key: &str) -> Result<&'a str, ProtocolError> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ProtocolError::Nostr(format!("projection field {key} is missing")))
}

/// Converts a verified standard NIP-25 event into Hydra's temporal reaction
/// record without inventing any additional public protocol.
///
/// # Errors
///
/// Returns an error for malformed target identifiers, authors, or reactions.
pub fn received_reaction(event: &Event) -> Result<Option<ReactionRecord>, ProtocolError> {
    event.verify().map_err(nostr_error)?;
    if event.kind != Kind::Reaction {
        return Ok(None);
    }
    let target = tag_value(event, "e").ok_or_else(|| missing_tag("e"))?;
    let value = match event.content.as_str() {
        "+" => ReactionValue::Upvote,
        "-" => ReactionValue::Downvote,
        "0" => ReactionValue::Neutral,
        other => ReactionValue::Emoji(other.to_owned()),
    };
    value.wire_value().map_err(domain_protocol_error)?;
    Ok(Some(ReactionRecord {
        actor: event_author(event)?,
        target: AnchorId::parse(target).map_err(domain_protocol_error)?,
        value,
        occurred_at: event.created_at.as_secs(),
        credited_reaffirmation: false,
        source_event_id: event.id.to_hex(),
    }))
}

fn native_thread_head(event: &Event) -> Result<ObjectHead, ProtocolError> {
    let communities = received_communities(event)?;
    let title = tag_value(event, "title")
        .filter(|value| !value.trim().is_empty())
        .ok_or(ProtocolError::EmptyTitle)?;
    validate_title(title)?;
    Ok(ObjectHead {
        anchor: AnchorId::parse(event.id.to_hex()).map_err(domain_protocol_error)?,
        author: event_author(event)?,
        kind: ObjectKind::Post,
        title: Some(title.to_owned()),
        body: ContentBody::parse(event.content.clone()).map_err(domain_protocol_error)?,
        communities,
        root: None,
        parent: None,
        external_root: tag_value(event, "i")
            .map(|value| hydra_domain::ExternalId::new("web", value))
            .transpose()
            .map_err(domain_protocol_error)?,
        external_parent: None,
        external_source: tag_value(event, "proxy")
            .map(|value| hydra_domain::ExternalId::new("web", value))
            .transpose()
            .map_err(domain_protocol_error)?,
        edited_at: event.created_at.as_secs(),
    })
}

fn native_comment_head(event: &Event) -> Result<ObjectHead, ProtocolError> {
    let communities = received_communities(event)?;
    let internal_root = tag_value(event, "E");
    let internal_parent = tag_value(event, "e");
    let external_root = tag_value(event, "I");
    let external_parent = tag_value(event, "i");
    let internal_thread = internal_root.is_some() && internal_parent.is_some();
    let external_thread =
        external_root.is_some() && (external_parent.is_some() || internal_parent.is_some());
    if !internal_thread && !external_thread {
        return Err(missing_tag("E/e or I/i"));
    }
    Ok(ObjectHead {
        anchor: AnchorId::parse(event.id.to_hex()).map_err(domain_protocol_error)?,
        author: event_author(event)?,
        kind: ObjectKind::Comment,
        title: None,
        body: ContentBody::parse(event.content.clone()).map_err(domain_protocol_error)?,
        communities,
        root: internal_root
            .map(AnchorId::parse)
            .transpose()
            .map_err(domain_protocol_error)?,
        parent: internal_parent
            .map(AnchorId::parse)
            .transpose()
            .map_err(domain_protocol_error)?,
        external_root: external_root
            .map(|value| hydra_domain::ExternalId::new("web", value))
            .transpose()
            .map_err(domain_protocol_error)?,
        external_parent: external_parent
            .map(|value| hydra_domain::ExternalId::new("web", value))
            .transpose()
            .map_err(domain_protocol_error)?,
        external_source: tag_value(event, "proxy")
            .map(|value| hydra_domain::ExternalId::new("web", value))
            .transpose()
            .map_err(domain_protocol_error)?,
        edited_at: event.created_at.as_secs(),
    })
}

fn open_nostr_head(event: &Event) -> Result<ObjectHead, ProtocolError> {
    let title = tag_value(event, "title")
        .filter(|value| validate_title(value).is_ok())
        .map(str::to_owned);
    let communities = event
        .tags
        .iter()
        .filter(|tag| tag.as_slice().first().map(String::as_str) == Some("t"))
        .filter_map(|tag| tag.content())
        .filter_map(|value| CommunityKey::parse(value).ok())
        .take(ObjectHead::MAX_COMMUNITIES)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(ObjectHead {
        anchor: AnchorId::parse(event.id.to_hex()).map_err(domain_protocol_error)?,
        author: event_author(event)?,
        kind: ObjectKind::Post,
        title,
        body: ContentBody::parse_post(event.content.clone()).map_err(domain_protocol_error)?,
        communities,
        root: None,
        parent: None,
        external_root: None,
        external_parent: None,
        external_source: None,
        edited_at: event.created_at.as_secs(),
    })
}

fn editable_received_head(
    event: &Event,
    current: Option<&ObjectHead>,
) -> Result<ObjectHead, ProtocolError> {
    let anchor = unique_tag_value(event, "e")?;
    let current = current
        .filter(|head| head.anchor.as_str() == anchor)
        .ok_or_else(|| {
            ProtocolError::Nostr("editable head arrived before its anchor".to_owned())
        })?;
    let author = event_author(event)?;
    if author != current.author {
        return Err(ProtocolError::Nostr(
            "editable head cannot change the anchor author".to_owned(),
        ));
    }
    let expected_kind = match current.kind {
        ObjectKind::Post | ObjectKind::Norm => Kind::Thread,
        ObjectKind::Comment => Kind::Comment,
    };
    let expected_address = format!("hydra:head:{anchor}");
    if unique_tag_value(event, "d")? != expected_address
        || unique_tag_value(event, "k")? != u16::from(expected_kind).to_string()
        || unique_tag_value(event, "L")? != "hydra"
        || unique_tag_value(event, "version")? != PROTOCOL_VERSION
        || !has_exact_tag(event, &["l", "object-head", "hydra"])
    {
        return Err(ProtocolError::Nostr(
            "editable head metadata does not match its immutable anchor".to_owned(),
        ));
    }
    let mut head = current.revised(
        ContentBody::parse(event.content.clone()).map_err(domain_protocol_error)?,
        event.created_at.as_secs(),
    );
    if current.kind != ObjectKind::Comment
        && let Some(title) = tag_value(event, "title")
    {
        validate_title(title)?;
        head.title = Some(title.to_owned());
    }
    let communities = received_communities(event)?;
    head.communities = communities;
    Ok(head)
}

fn received_communities(event: &Event) -> Result<Vec<CommunityKey>, ProtocolError> {
    let communities = event
        .tags
        .iter()
        .filter(|tag| tag.as_slice().first().map(String::as_str) == Some("t"))
        .filter_map(|tag| tag.content())
        .map(CommunityKey::parse)
        .collect::<Result<Vec<_>, _>>()
        .map_err(domain_protocol_error)?;
    require_communities(&communities)?;
    Ok(communities)
}

fn validate_title(title: &str) -> Result<(), ProtocolError> {
    if title.trim().is_empty() {
        return Err(ProtocolError::EmptyTitle);
    }
    if title.len() > ObjectHead::MAX_TITLE_LEN || title.chars().any(char::is_control) {
        return Err(ProtocolError::Nostr(format!(
            "title exceeds {} bytes or contains control characters",
            ObjectHead::MAX_TITLE_LEN
        )));
    }
    Ok(())
}

fn tag_value<'a>(event: &'a Event, name: &str) -> Option<&'a str> {
    event.tags.iter().find_map(|tag| {
        (tag.as_slice().first().map(String::as_str) == Some(name))
            .then(|| tag.content())
            .flatten()
    })
}

fn unique_tag_value<'a>(event: &'a Event, name: &str) -> Result<&'a str, ProtocolError> {
    let mut matches = event.tags.iter().filter_map(|tag| {
        (tag.as_slice().first().map(String::as_str) == Some(name))
            .then(|| tag.as_slice().get(1).map(String::as_str))
            .flatten()
    });
    let value = matches.next().ok_or_else(|| missing_tag(name))?;
    if matches.next().is_some() {
        return Err(ProtocolError::Nostr(format!(
            "duplicate required {name} tag"
        )));
    }
    Ok(value)
}

fn has_exact_tag(event: &Event, expected: &[&str]) -> bool {
    event.tags.iter().any(|tag| {
        tag.as_slice()
            .iter()
            .map(String::as_str)
            .eq(expected.iter().copied())
    })
}

fn append_comment_scope_tags(
    tags: &mut Vec<Tag>,
    scope: Option<CommentScope>,
) -> Result<(), ProtocolError> {
    let Some(scope) = scope else {
        return Ok(());
    };
    tags.extend([
        tag(["E", &scope.root.id.to_hex()])?,
        tag(["K", &u16::from(scope.root.kind).to_string()])?,
        tag(["P", &scope.root.author.to_hex()])?,
        tag([
            "parent",
            &scope.parent.id.to_hex(),
            &u16::from(scope.parent.kind).to_string(),
            &scope.parent.author.to_hex(),
        ])?,
    ]);
    Ok(())
}

fn event_author(event: &Event) -> Result<NostrPublicKey, ProtocolError> {
    NostrPublicKey::parse(event.pubkey.to_bech32().map_err(nostr_error)?)
        .map_err(domain_protocol_error)
}

fn missing_tag(tag: &str) -> ProtocolError {
    ProtocolError::Nostr(format!("missing required {tag} tag"))
}

fn domain_protocol_error(error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::Nostr(error.to_string())
}

/// Encrypts persona-private local state to the persona itself using NIP-44.
///
/// # Errors
///
/// Returns an error when the persona lacks a secret key or encryption fails.
pub fn encrypt_private(keys: &Keys, plaintext: &str) -> Result<String, ProtocolError> {
    nip44::encrypt(
        keys.secret_key(),
        &keys.public_key(),
        plaintext,
        nip44::Version::V2,
    )
    .map_err(nostr_error)
}

/// Decrypts persona-private local state encrypted to the persona itself.
///
/// # Errors
///
/// Returns an error when the persona lacks the corresponding key or the
/// ciphertext is malformed or unauthentic.
pub fn decrypt_private(keys: &Keys, ciphertext: &str) -> Result<String, ProtocolError> {
    nip44::decrypt(keys.secret_key(), &keys.public_key(), ciphertext).map_err(nostr_error)
}

/// Builds the two NIP-17 gift wraps required for a recoverable direct message:
/// one for the recipient and one for the sender.
///
/// # Errors
///
/// Returns an error for an invalid recipient or failed NIP-44/NIP-59 wrapping.
pub async fn private_message<S>(
    signer: &S,
    recipient: &NostrPublicKey,
    body: &str,
    created_at: u64,
) -> Result<PrivateMessageWraps, ProtocolError>
where
    S: NostrSigner,
{
    if body.trim().is_empty() {
        return Err(ProtocolError::Nostr("message cannot be empty".to_owned()));
    }
    let recipient = PublicKey::parse(recipient.as_str()).map_err(nostr_error)?;
    let mut rumor = EventBuilder::private_msg_rumor(recipient, body)
        .custom_created_at(Timestamp::from(created_at))
        .build(signer.get_public_key().await.map_err(nostr_error)?);
    let rumor_id = rumor.id().to_hex();
    let recipient_wrap = EventBuilder::gift_wrap(signer, &recipient, rumor.clone(), [])
        .await
        .map_err(nostr_error)?;
    let sender_public_key = signer.get_public_key().await.map_err(nostr_error)?;
    let sender_copy = EventBuilder::gift_wrap(signer, &sender_public_key, rumor, [])
        .await
        .map_err(nostr_error)?;
    Ok(PrivateMessageWraps {
        rumor_id,
        recipient: recipient_wrap,
        sender_copy,
    })
}

/// Authenticates and unwraps one NIP-17 direct-message gift wrap.
///
/// # Errors
///
/// Returns an error when the gift wrap is not addressed to the selected
/// persona, cannot be authenticated, or does not contain a kind-14 message.
pub async fn unwrap_private_message<S>(
    signer: &S,
    gift_wrap: &Event,
) -> Result<UnwrappedDirectMessage, ProtocolError>
where
    S: NostrSigner,
{
    let UnwrappedGift { sender, mut rumor } = UnwrappedGift::from_gift_wrap(signer, gift_wrap)
        .await
        .map_err(nostr_error)?;
    if rumor.kind != Kind::PrivateDirectMessage {
        return Err(ProtocolError::Nostr(
            "gift wrap does not contain a direct message".to_owned(),
        ));
    }
    let rumor_id = rumor.id().to_hex();
    let recipients = rumor
        .tags
        .iter()
        .filter(|tag| tag.as_slice().first().map(String::as_str) == Some("p"))
        .filter_map(|tag| tag.content())
        .map(|value| {
            let key = PublicKey::parse(value).map_err(nostr_error)?;
            NostrPublicKey::parse(key.to_bech32().map_err(nostr_error)?)
                .map_err(domain_protocol_error)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(UnwrappedDirectMessage {
        rumor_id,
        sender: NostrPublicKey::parse(sender.to_bech32().map_err(nostr_error)?)
            .map_err(|error| ProtocolError::Nostr(error.to_string()))?,
        recipients,
        body: rumor.content,
        created_at: rumor.created_at.as_secs(),
    })
}

/// Builds the persona's current public NIP-02 follow list.
///
/// # Errors
///
/// Returns an error for an invalid public key or signing failure.
pub fn public_follow_list(
    signer: &impl EventSigner,
    targets: &[NostrPublicKey],
    changed_at: u64,
) -> Result<Event, ProtocolError> {
    let contacts = targets
        .iter()
        .map(|target| PublicKey::parse(target.as_str()).map(Contact::new))
        .collect::<Result<Vec<_>, _>>()
        .map_err(nostr_error)?;
    EventBuilder::contact_list(contacts)
        .custom_created_at(Timestamp::from(changed_at))
        .sign_with(signer)
}

/// Builds one standard public NIP-51 curated follow set.
///
/// # Errors
///
/// Returns an error for an invalid public key or signing failure.
pub fn public_follow_set(
    signer: &impl EventSigner,
    identifier: &str,
    title: &str,
    targets: &[NostrPublicKey],
    changed_at: u64,
) -> Result<Event, ProtocolError> {
    let public_keys = targets
        .iter()
        .map(|target| PublicKey::parse(target.as_str()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(nostr_error)?;
    EventBuilder::follow_set(identifier, public_keys)
        .tag(Tag::title(title))
        .custom_created_at(Timestamp::from(changed_at))
        .sign_with(signer)
}

/// Builds a standard NIP-09 request covering an immutable Hydra anchor and
/// its addressable editable head. Relays may ignore this request.
///
/// # Errors
///
/// Returns an error for an invalid anchor or signing failure.
pub fn object_deletion_request(
    signer: &impl EventSigner,
    anchor: &AnchorId,
    reason: &str,
    requested_at: u64,
) -> Result<Event, ProtocolError> {
    let anchor_id = EventId::parse(anchor.as_str()).map_err(nostr_error)?;
    let coordinate = Coordinate::new(Kind::Custom(OBJECT_HEAD_KIND), signer.public_key())
        .identifier(format!("hydra:head:{}", anchor.as_str()));
    EventBuilder::delete(
        EventDeletionRequest::new()
            .id(anchor_id)
            .coordinate(coordinate)
            .reason(reason.trim()),
    )
    .custom_created_at(Timestamp::from(requested_at))
    .sign_with(signer)
}

/// Builds the persona's current public NIP-51 topic-interest list.
///
/// # Errors
///
/// Returns an error when signing fails.
pub fn public_interests(
    signer: &impl EventSigner,
    communities: &[CommunityKey],
    changed_at: u64,
) -> Result<Event, ProtocolError> {
    EventBuilder::interests(Interests {
        hashtags: communities
            .iter()
            .map(|community| community.as_str().to_owned())
            .collect(),
        coordinate: Vec::new(),
    })
    .custom_created_at(Timestamp::from(changed_at))
    .sign_with(signer)
}

/// Builds the persona's current public NIP-51 mute list.
///
/// # Errors
///
/// Returns an error for an invalid public key or signing failure.
pub fn public_mute_list(
    signer: &impl EventSigner,
    targets: &[NostrPublicKey],
    changed_at: u64,
) -> Result<Event, ProtocolError> {
    let public_keys = targets
        .iter()
        .map(|target| PublicKey::parse(target.as_str()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(nostr_error)?;
    EventBuilder::mute_list(MuteList {
        public_keys,
        ..MuteList::default()
    })
    .custom_created_at(Timestamp::from(changed_at))
    .sign_with(signer)
}

/// Builds a NIP-32 public claim explaining one block declaration.
///
/// # Errors
///
/// Returns an error for an invalid public key or signing failure.
pub fn public_block_reason(
    signer: &impl EventSigner,
    target: &NostrPublicKey,
    reason: &str,
    changed_at: u64,
) -> Result<Event, ProtocolError> {
    let target = PublicKey::parse(target.as_str()).map_err(nostr_error)?;
    EventBuilder::label("hydra:block", reason)
        .tag(Tag::public_key(target))
        .custom_created_at(Timestamp::from(changed_at))
        .sign_with(signer)
}

/// Builds a NIP-32 label identifying a thread anchor as a Hydra norm proposal.
///
/// # Errors
///
/// Returns an error for invalid tags or signing failure.
pub fn norm_label(
    signer: &impl EventSigner,
    anchor: EventReference,
    created_at: u64,
) -> Result<Event, ProtocolError> {
    EventBuilder::label("hydra:object", "community-norm")
        .tag(Tag::event(anchor.id))
        .tag(Tag::public_key(anchor.author))
        .custom_created_at(Timestamp::from(created_at))
        .sign_with(signer)
}

fn require_communities(communities: &[CommunityKey]) -> Result<(), ProtocolError> {
    if communities.is_empty() {
        Err(ProtocolError::MissingCommunity)
    } else {
        Ok(())
    }
}

fn community_tags(communities: &[CommunityKey]) -> Result<Vec<Tag>, ProtocolError> {
    communities
        .iter()
        .map(|community| tag(["t", community.as_str()]))
        .collect()
}

fn tag<const N: usize>(parts: [&str; N]) -> Result<Tag, ProtocolError> {
    Tag::parse(parts).map_err(nostr_error)
}

fn nostr_error(error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::Nostr(error.to_string())
}

#[cfg(test)]
mod tests {
    use hydra_domain::{PersonaId, ProjectionId, ProjectionState};
    use nostr::{FromBech32, JsonUtil, ToBech32, nips::nip19::Nip19Event};

    use super::*;

    #[test]
    fn portable_uri_contains_event_identity_and_relay_hints() {
        let keys = Keys::generate();
        let event = post_anchor(&keys, "Title", "Body", &communities(), 10).unwrap();
        let uri = portable_event_uri(&event, &["wss://relay.example".to_owned()]).unwrap();
        let encoded = uri.strip_prefix("nostr:").unwrap();
        let entity = Nip19Event::from_bech32(encoded).unwrap();

        assert_eq!(entity.event_id, event.id);
        assert_eq!(entity.author, Some(event.pubkey));
        assert_eq!(entity.kind, Some(event.kind));
        assert_eq!(entity.relays[0].as_str(), "wss://relay.example");
    }

    #[test]
    fn inbox_declaration_uses_standard_nip_17_kind_and_relay_tags() {
        let keys = Keys::generate();
        let event = inbox_relays(&keys, &["wss://inbox.example".to_owned()], 10).unwrap();

        assert_eq!(event.kind, Kind::InboxRelays);
        assert!(
            event
                .as_json()
                .contains("[\"relay\",\"wss://inbox.example\"]")
        );
    }

    #[test]
    fn blossom_authorization_and_file_metadata_use_existing_standards() {
        let keys = Keys::generate();
        let hash = "a".repeat(64);
        let authorization =
            blossom_upload_authorization(&keys, "blobs.example", &hash, 10).unwrap();
        let encoded = authorization.strip_prefix("Nostr ").unwrap();
        let json = String::from_utf8(URL_SAFE_NO_PAD.decode(encoded).unwrap()).unwrap();
        let token = Event::from_json(json).unwrap();
        assert_eq!(token.kind, Kind::Custom(24_242));
        assert!(token.as_json().contains("[\"t\",\"upload\"]"));
        assert!(token.as_json().contains("[\"server\",\"blobs.example\"]"));
        assert!(token.as_json().contains(&hash));

        let manifest = MediaManifest {
            object: AnchorId::parse("anchor").unwrap(),
            sha256: hash.clone(),
            mime_type: "image/png".to_owned(),
            size: 42,
            dimensions: Some("10x20".to_owned()),
            duration_seconds: None,
            local_path: format!("media/{hash}"),
            original_url: None,
            blob_urls: vec![
                format!("https://blobs.example/{hash}.png"),
                format!("https://backup.example/{hash}.png"),
            ],
            metadata_event_id: None,
            preserved_at: 10,
        };
        let metadata = file_metadata(&keys, &manifest, "Hydra image", 11).unwrap();
        let metadata_json = metadata.as_json();
        assert_eq!(metadata.kind, Kind::Custom(1063));
        assert!(metadata_json.contains("[\"m\",\"image/png\"]"));
        assert!(metadata_json.contains("[\"dim\",\"10x20\"]"));
        assert!(metadata_json.contains("[\"fallback\""));
        assert!(metadata_json.contains(&hash));

        let anchor = post_anchor(&keys, "Image", "body", &communities(), 12).unwrap();
        let body = format!("body\n\n{}", manifest.blob_urls[0]);
        let head = object_head_with_media(
            &keys,
            EventReference {
                id: anchor.id,
                kind: anchor.kind,
                author: anchor.pubkey,
            },
            None,
            Some("Image"),
            &body,
            &communities(),
            &[manifest],
            13,
        )
        .unwrap();
        assert!(
            head.as_json()
                .contains("[\"imeta\",\"url https://blobs.example/")
        );
    }

    fn communities() -> Vec<CommunityKey> {
        vec![
            CommunityKey::parse("science").unwrap(),
            CommunityKey::parse("biology").unwrap(),
        ]
    }

    #[test]
    fn post_is_one_thread_in_multiple_communities() {
        let keys = Keys::generate();
        let event = post_anchor(&keys, "Fungi", "One organism", &communities(), 10).unwrap();
        let json = event.as_json();
        assert_eq!(event.kind, Kind::Thread);
        assert!(json.contains("[\"title\",\"Fungi\"]"));
        assert!(json.contains("[\"t\",\"science\"]"));
        assert!(json.contains("[\"t\",\"biology\"]"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delegated_signer_builds_valid_hydra_events_without_local_key_access() {
        let remote_keys = Keys::generate();
        let signer = HydraSigner::remote(remote_keys.public_key(), Arc::new(remote_keys));
        let event =
            post_anchor(&signer, "Delegated", "Signed elsewhere", &communities(), 10).unwrap();
        event.verify().unwrap();
        assert_eq!(event.pubkey, signer.public_key());
    }

    #[test]
    fn edits_replace_one_addressable_head_without_changing_anchor() {
        let keys = Keys::generate();
        let anchor = post_anchor(&keys, "Fungi", "First", &communities(), 10).unwrap();
        let reference = EventReference {
            id: anchor.id,
            kind: anchor.kind,
            author: anchor.pubkey,
        };
        let first = object_head(
            &keys,
            reference,
            None,
            Some("Fungi"),
            "First",
            &communities(),
            10,
        )
        .unwrap();
        let second = object_head(
            &keys,
            reference,
            None,
            Some("Fungi revisited"),
            "Second",
            &communities(),
            20,
        )
        .unwrap();

        assert_eq!(first.kind, Kind::Custom(OBJECT_HEAD_KIND));
        assert_ne!(first.id, second.id);
        let expected = format!("hydra:head:{}", anchor.id.to_hex());
        assert!(first.as_json().contains(&expected));
        assert!(second.as_json().contains(&expected));
        assert!(second.as_json().contains("Fungi revisited"));
    }

    #[test]
    fn received_events_materialize_remote_nostr_authors_and_edits() {
        let keys = Keys::generate();
        let anchor = post_anchor(&keys, "Fungi", "Original", &communities(), 10).unwrap();
        let initial = received_object_head(&anchor, None).unwrap().unwrap();
        let revision = object_head(
            &keys,
            EventReference {
                id: anchor.id,
                kind: anchor.kind,
                author: anchor.pubkey,
            },
            None,
            Some("Fungi revised"),
            "Edited",
            &communities(),
            20,
        )
        .unwrap();
        let edited = received_object_head(&revision, Some(&initial))
            .unwrap()
            .unwrap();

        assert_eq!(edited.anchor, initial.anchor);
        assert_eq!(
            edited.author.as_str(),
            keys.public_key().to_bech32().unwrap()
        );
        assert_eq!(edited.body.as_str(), "Edited");
        assert_eq!(edited.title.as_deref(), Some("Fungi revised"));
    }

    #[test]
    fn native_comment_heads_copy_root_and_parent_metadata() {
        let keys = Keys::generate();
        let root = post_anchor(&keys, "Root", "Root body", &communities(), 10).unwrap();
        let root_reference = EventReference {
            id: root.id,
            kind: root.kind,
            author: root.pubkey,
        };
        let anchor = comment_anchor(
            &keys,
            "Reply",
            CommentScope {
                root: root_reference,
                parent: root_reference,
            },
            &communities(),
            11,
        )
        .unwrap();
        let head = object_head(
            &keys,
            EventReference {
                id: anchor.id,
                kind: anchor.kind,
                author: anchor.pubkey,
            },
            Some(CommentScope {
                root: root_reference,
                parent: root_reference,
            }),
            None,
            "Reply",
            &communities(),
            11,
        )
        .unwrap();
        let json = head.as_json();
        assert!(json.contains(&format!("[\"E\",\"{}\"]", root.id.to_hex())));
        assert!(json.contains("[\"K\",\"11\"]"));
        assert!(json.contains(&format!(
            "[\"parent\",\"{}\",\"11\",\"{}\"]",
            root.id.to_hex(),
            root.pubkey.to_hex()
        )));
    }

    #[test]
    fn editable_heads_reject_wrong_or_duplicate_required_metadata() {
        let keys = Keys::generate();
        let anchor = post_anchor(&keys, "Root", "Original", &communities(), 10).unwrap();
        let current = received_object_head(&anchor, None).unwrap().unwrap();
        let common = vec![
            tag(["d", &format!("hydra:head:{}", anchor.id.to_hex())]).unwrap(),
            tag(["e", &anchor.id.to_hex()]).unwrap(),
            tag(["k", "11"]).unwrap(),
            tag(["L", "hydra"]).unwrap(),
            tag(["l", "object-head", "hydra"]).unwrap(),
            tag(["t", "science"]).unwrap(),
        ];
        let mut wrong_version_tags = common.clone();
        wrong_version_tags.push(tag(["version", "hydra-protocol/v999"]).unwrap());
        let wrong_version = EventBuilder::new(Kind::Custom(OBJECT_HEAD_KIND), "Edited")
            .tags(wrong_version_tags)
            .custom_created_at(Timestamp::from(20))
            .sign_with(&keys)
            .unwrap();
        assert!(received_object_head(&wrong_version, Some(&current)).is_err());

        let mut duplicate_anchor_tags = common;
        duplicate_anchor_tags.push(tag(["e", &anchor.id.to_hex()]).unwrap());
        duplicate_anchor_tags.push(tag(["version", PROTOCOL_VERSION]).unwrap());
        let duplicate_anchor = EventBuilder::new(Kind::Custom(OBJECT_HEAD_KIND), "Edited")
            .tags(duplicate_anchor_tags)
            .custom_created_at(Timestamp::from(20))
            .sign_with(&keys)
            .unwrap();
        assert!(received_object_head(&duplicate_anchor, Some(&current)).is_err());
    }

    #[test]
    fn community_refresh_filter_is_bounded_and_topic_scoped() {
        let filter = community_filter(&[
            CommunityKey::parse("science").unwrap(),
            CommunityKey::parse("biology").unwrap(),
        ]);
        let json = filter.as_json();

        assert!(json.contains("science"));
        assert!(json.contains("biology"));
        assert!(json.contains("30800"));
        assert!(json.contains("\"limit\":500"));
    }

    #[test]
    fn open_nostr_notes_keep_real_topics_and_allow_uncategorized_content() {
        let keys = Keys::generate();
        let tagged = EventBuilder::new(Kind::TextNote, "Tagged note")
            .tags([tag(["t", "science"]).unwrap()])
            .custom_created_at(Timestamp::from(10))
            .sign_with(&keys)
            .unwrap();
        let untagged = EventBuilder::new(Kind::TextNote, "General note")
            .custom_created_at(Timestamp::from(11))
            .sign_with(&keys)
            .unwrap();

        let tagged_head = received_object_head(&tagged, None).unwrap().unwrap();
        let untagged_head = received_object_head(&untagged, None).unwrap().unwrap();
        assert_eq!(
            tagged_head.communities,
            vec![CommunityKey::parse("science").unwrap()]
        );
        assert!(untagged_head.communities.is_empty());
        assert_eq!(untagged_head.body.as_str(), "General note");
    }

    #[test]
    fn open_nostr_feed_excludes_context_free_protocol_events() {
        let kinds = open_content_kinds();

        assert!(kinds.contains(&Kind::TextNote));
        assert!(kinds.contains(&Kind::Thread));
        assert!(kinds.contains(&Kind::LongFormTextNote));
        assert!(!kinds.contains(&Kind::Reaction));
        assert!(!kinds.contains(&Kind::Comment));
        assert!(!kinds.contains(&Kind::Repost));
        assert!(!kinds.contains(&Kind::Custom(OBJECT_HEAD_KIND)));
    }

    #[test]
    fn curation_repost_preserves_the_source_and_adds_topics() {
        let author = Keys::generate();
        let curator = Keys::generate();
        let source = EventBuilder::new(Kind::TextNote, "Original words")
            .custom_created_at(Timestamp::from(10))
            .sign_with(&author)
            .unwrap();
        let repost = curation_repost(&curator, &source, &communities(), 20).unwrap();
        let json = repost.as_json();

        assert_eq!(repost.pubkey, curator.public_key());
        assert_ne!(repost.pubkey, source.pubkey);
        assert!(json.contains(&source.id.to_hex()));
        assert!(json.contains("[\"t\",\"science\"]"));
        assert_eq!(repost.content, source.as_json());
    }

    #[test]
    fn nested_comment_keeps_root_and_immediate_parent() {
        let root_keys = Keys::generate();
        let reply_keys = Keys::generate();
        let root = post_anchor(&root_keys, "Fungi", "Root", &communities(), 10).unwrap();
        let root_reference = EventReference {
            id: root.id,
            kind: root.kind,
            author: root.pubkey,
        };
        let parent = comment_anchor(
            &reply_keys,
            "Parent",
            CommentScope {
                root: root_reference,
                parent: root_reference,
            },
            &communities(),
            11,
        )
        .unwrap();
        let child = comment_anchor(
            &root_keys,
            "Child",
            CommentScope {
                root: root_reference,
                parent: EventReference {
                    id: parent.id,
                    kind: parent.kind,
                    author: parent.pubkey,
                },
            },
            &communities(),
            12,
        )
        .unwrap();
        let json = child.as_json();

        assert_eq!(child.kind, Kind::Comment);
        assert!(json.contains(&format!("[\"E\",\"{}\"]", root.id.to_hex())));
        assert!(json.contains(&format!("[\"e\",\"{}\"]", parent.id.to_hex())));
        assert!(json.contains("[\"K\",\"11\"]"));
        assert!(json.contains("[\"k\",\"1111\"]"));
    }

    #[test]
    fn external_comment_and_reaction_reuse_nip_22_nip_25_and_nip_73() {
        let keys = Keys::generate();
        let root = hydra_domain::ExternalId::new(
            "reddit",
            "https://www.reddit.com/r/science/comments/root/title/",
        )
        .unwrap();
        let parent = hydra_domain::ExternalId::new(
            "reddit",
            "https://www.reddit.com/r/science/comments/root/title/comment/",
        )
        .unwrap();
        let scope = ExternalCommentScope {
            root: root.clone(),
            parent: parent.clone(),
        };
        let source = hydra_domain::ExternalId::new(
            "reddit",
            "https://www.reddit.com/r/science/comments/root/title/comment/?context=3",
        )
        .unwrap();
        let comment = external_comment_anchor(
            &keys,
            "Hydra reply",
            &scope,
            Some(&source),
            &communities(),
            20,
        )
        .unwrap();
        let reaction = external_reaction(&keys, &parent, "+", 21).unwrap();
        assert_eq!(comment.kind, Kind::Comment);
        assert!(
            comment
                .as_json()
                .contains("[\"I\",\"https://www.reddit.com/r/science/comments/root/title/\"]")
        );
        assert!(
            comment.as_json().contains(
                "[\"i\",\"https://www.reddit.com/r/science/comments/root/title/comment/\"]"
            )
        );
        assert!(comment.as_json().contains(
            "[\"proxy\",\"https://www.reddit.com/r/science/comments/root/title/comment/?context=3\",\"web\"]"
        ));
        assert_eq!(reaction.kind, Kind::Custom(17));
        assert!(reaction.as_json().contains("[\"k\",\"web\"]"));
        assert!(reaction.as_json().contains(&parent.canonical));
    }

    #[test]
    fn vote_targets_the_stable_anchor() {
        let keys = Keys::generate();
        let post = post_anchor(&keys, "Fungi", "Root", &communities(), 10).unwrap();
        let vote = reaction(
            &keys,
            EventReference {
                id: post.id,
                kind: post.kind,
                author: post.pubkey,
            },
            "+",
            20,
        )
        .unwrap();

        assert_eq!(vote.kind, Kind::Reaction);
        assert_eq!(vote.content, "+");
        assert!(vote.as_json().contains(&post.id.to_hex()));
    }

    #[test]
    fn projection_revisions_share_one_addressable_identifier() {
        let keys = Keys::generate();
        let mut projection = Projection {
            id: ProjectionId::new(),
            anchor: hydra_domain::AnchorId::parse(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap(),
            destination: hydra_domain::ExternalId::new("reddit-community", "science").unwrap(),
            external_id: Some(hydra_domain::ExternalId::new("reddit", "t3_example").unwrap()),
            external_url: Some("https://www.reddit.com/r/science/comments/example".to_owned()),
            persona: PersonaId::new(),
            state: ProjectionState::Live,
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
        let first = projection_record(&keys, &projection, 20).unwrap();
        projection.state = ProjectionState::Removed;
        let second = projection_record(&keys, &projection, 21).unwrap();
        let identifier = first
            .tags
            .iter()
            .find(|tag| tag.kind() == nostr::TagKind::d())
            .unwrap()
            .content()
            .unwrap()
            .to_owned();

        assert_ne!(first.id, second.id);
        assert!(identifier.starts_with("hydra:projection:"));
        assert!(first.as_json().contains(&identifier));
        assert!(second.as_json().contains(&identifier));
        assert!(
            second
                .content
                .contains("\"reddit_fullname\":\"t3_example\"")
        );
        assert!(second.content.contains("\"target_subreddit\":\"science\""));
        assert!(second.content.contains("\"projection_type\":\"post\""));
        let received = received_projection_record(&second).unwrap().unwrap();
        assert_eq!(received.state, "removed");
        assert_eq!(received.target_subreddit, "science");
        assert_eq!(received.reddit_fullname, "t3_example");
        assert_eq!(
            received.author.as_str(),
            keys.public_key().to_bech32().unwrap()
        );

        let mut malformed = serde_json::from_str::<serde_json::Value>(&second.as_json()).unwrap();
        malformed["content"] =
            serde_json::Value::String(second.content.replace("\"science\"", "\"not-science\""));
        let malformed = Event::from_json(malformed.to_string()).unwrap();
        assert!(received_projection_record(&malformed).is_err());
        assert!(
            projection_record(
                &keys,
                &Projection {
                    external_id: Some(
                        hydra_domain::ExternalId::new("reddit", "t3_different").unwrap(),
                    ),
                    ..projection.clone()
                },
                22,
            )
            .is_err()
        );
        assert!(!second.content.contains("display_error"));
        assert!(!second.content.contains("rendered_payload"));
        assert!(
            projection_record(
                &keys,
                &Projection {
                    state: ProjectionState::Failed,
                    ..projection
                },
                22,
            )
            .is_err()
        );
    }

    #[test]
    fn social_lists_use_nip_02_nip_51_and_nip_32() {
        let keys = Keys::generate();
        let target_keys = Keys::generate();
        let target = NostrPublicKey::parse(target_keys.public_key().to_bech32().unwrap()).unwrap();
        let follows = public_follow_list(&keys, std::slice::from_ref(&target), 20).unwrap();
        let follow_set = public_follow_set(
            &keys,
            "recommended",
            "Recommended personas",
            std::slice::from_ref(&target),
            20,
        )
        .unwrap();
        let mutes = public_mute_list(&keys, std::slice::from_ref(&target), 21).unwrap();
        let reason = public_block_reason(&keys, &target, "Persistent abuse", 22).unwrap();

        assert_eq!(follows.kind, Kind::ContactList);
        assert_eq!(follow_set.kind, Kind::FollowSet);
        assert!(follow_set.as_json().contains("Recommended personas"));
        assert!(follow_set.as_json().contains("recommended"));
        assert_eq!(mutes.kind, Kind::MuteList);
        assert_eq!(reason.kind, Kind::Label);
        assert!(reason.as_json().contains("hydra:block"));
        assert!(
            reason
                .as_json()
                .contains(&target_keys.public_key().to_hex())
        );
        let interests = public_interests(&keys, &communities(), 23).unwrap();
        assert_eq!(interests.kind, Kind::Interests);
        assert!(interests.as_json().contains("[\"t\",\"science\"]"));
    }

    #[test]
    fn disowning_uses_standard_nip_09_for_anchor_and_head() {
        let keys = Keys::generate();
        let anchor = AnchorId::parse("a".repeat(64)).unwrap();
        let request = object_deletion_request(&keys, &anchor, "withdrawn", 24).unwrap();
        let json = request.as_json();

        assert_eq!(request.kind, Kind::EventDeletion);
        assert!(json.contains(&format!("[\"e\",\"{}\"]", anchor.as_str())));
        assert!(json.contains("hydra:head:"));
        assert!(json.contains("withdrawn"));
    }

    #[test]
    fn persona_relays_use_standard_nip_65_direction_markers() {
        let keys = Keys::generate();
        let event = persona_relay_list(
            &keys,
            &[
                "wss://both.example".to_owned(),
                "wss://read.example".to_owned(),
            ],
            &[
                "wss://both.example".to_owned(),
                "wss://write.example".to_owned(),
            ],
            20,
        )
        .unwrap();
        let json = event.as_json();

        assert_eq!(event.kind, Kind::RelayList);
        assert!(json.contains("[\"r\",\"wss://both.example\"]"));
        assert!(json.contains("[\"r\",\"wss://read.example\",\"read\"]"));
        assert!(json.contains("[\"r\",\"wss://write.example\",\"write\"]"));
    }

    #[test]
    fn persona_profile_uses_standard_nip_01_metadata() {
        let keys = Keys::generate();
        let event = profile_metadata(&keys, "Alice", 20).unwrap();

        assert_eq!(event.kind, Kind::Metadata);
        assert_eq!(event.pubkey, keys.public_key());
        assert!(event.content.contains("\"display_name\":\"Alice\""));
        assert!(event.content.contains("\"name\":\"Alice\""));
    }

    #[test]
    fn reddit_identity_proof_uses_nip_39_extensible_identity_tag() {
        let keys = Keys::generate();
        let proof = hydra_domain::RedditIdentityProof {
            persona: PersonaId::new(),
            username: "alice".to_owned(),
            artifact_url: "https://www.reddit.com/r/test/comments/abc/proof/def/".to_owned(),
            published_at: 20,
        };
        let event = reddit_identity_proof(&keys, &proof).unwrap();

        assert_eq!(event.kind, Kind::Custom(10_011));
        assert!(event.as_json().contains(
            "[\"i\",\"reddit:alice\",\"https://www.reddit.com/r/test/comments/abc/proof/def/\"]"
        ));
    }

    #[test]
    fn norm_is_classified_with_a_standard_label() {
        let keys = Keys::generate();
        let anchor = post_anchor(&keys, "Community norm", "Be kind", &communities(), 20).unwrap();
        let label = norm_label(
            &keys,
            EventReference {
                id: anchor.id,
                kind: anchor.kind,
                author: anchor.pubkey,
            },
            20,
        )
        .unwrap();

        assert_eq!(label.kind, Kind::Label);
        assert!(label.as_json().contains("community-norm"));
        assert!(label.as_json().contains(&anchor.id.to_hex()));
    }

    #[test]
    fn private_local_payloads_are_nip_44_authenticated_and_persona_scoped() {
        let alice = Keys::generate();
        let bob = Keys::generate();
        let ciphertext = encrypt_private(&alice, "private memory").unwrap();

        assert_ne!(ciphertext, "private memory");
        assert_eq!(
            decrypt_private(&alice, &ciphertext).unwrap(),
            "private memory"
        );
        assert!(decrypt_private(&bob, &ciphertext).is_err());
    }

    #[tokio::test]
    async fn direct_message_has_receiver_and_sender_nip_17_copies() {
        let alice = Keys::generate();
        let bob = Keys::generate();
        let bob_key = NostrPublicKey::parse(bob.public_key().to_bech32().unwrap()).unwrap();
        let wraps = private_message(&alice, &bob_key, "hello", 42)
            .await
            .unwrap();

        assert_eq!(wraps.recipient.kind, Kind::GiftWrap);
        assert_eq!(wraps.sender_copy.kind, Kind::GiftWrap);
        assert_ne!(wraps.recipient.id, wraps.sender_copy.id);
        let received = unwrap_private_message(&bob, &wraps.recipient)
            .await
            .unwrap();
        let recovered = unwrap_private_message(&alice, &wraps.sender_copy)
            .await
            .unwrap();
        assert_eq!(received.rumor_id, wraps.rumor_id);
        assert_eq!(received, recovered);
        assert_eq!(received.body, "hello");
        assert_eq!(received.created_at, 42);
        assert_eq!(received.recipients, vec![bob_key]);
    }

    #[tokio::test]
    async fn empty_relay_probe_is_honest_and_side_effect_free() {
        let probe = probe_relays(&[], Duration::from_millis(1)).await.unwrap();
        assert_eq!(probe.configured, 0);
        assert_eq!(probe.connected, 0);
    }
}
