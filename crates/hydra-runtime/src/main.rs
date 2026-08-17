#![forbid(unsafe_code)]

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{BufRead, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::{Duration, Instant},
};

use hydra_app::{
    ArchiveService, BackupService, CreateComment, CreateExternalComment, CreateNorm, CreatePost,
    CurateEvent, DiscussionService, DraftService, EditObject, ImportAuthoredPost, ImportService,
    MessagingService, PersonaService, PlatformSecretStore, PreserveAndPublishMedia, PrivateState,
    ProjectionService, PublishFollowSet, ReactToObject, RemoveRevisit, RequestObjectDisowning,
    SendDirectMessage, SetBlock, SetCommunitySubscription, SetFollow, SetLocalFilter, SetRevisit,
    SocialService, SyncService, private_state,
};
use hydra_domain::{
    AnchorId, CommunityKey, ContinuityState, ContinuityWorkflow, DraftKind, DraftRecord,
    DurableEvent, ExternalId, LocalFilterKind, MessageDirection, NostrPublicKey, OperationId,
    OperationState, PersonaId, PersonaSwitchState, PreservationLevel, ProjectionId, ReactionValue,
    RevisitIntent,
};
use hydra_lens::{FeedLens, FeedService};
use hydra_media::MediaStore;
use hydra_nostr::SdkEventPublisher;
use hydra_reddit::{
    Attribution, BigStickAction, BridgeError, BridgeService, OAuthTokens,
    PlatformRedditCredentialStore, ProjectionAction, QueueComment, QueuePost, RedditCredential,
    RedditCredentialStore, RedditDataApi, RedditError, RedditFullname, RedditLinkService,
    ResolveDuplicatesAction, WithdrawalAction, WithdrawalMarker,
};
use hydra_store::{DurableStore, HeadStore, ReadinessProbe, Settings, SettingsStore, StoreError};
use nostr::{Event, JsonUtil};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
enum RuntimeError {
    #[error("missing or unknown command")]
    UnknownCommand,
    #[error("action requires a JSON input argument")]
    MissingInput,
    #[error("unknown action: {0}")]
    UnknownAction(String),
    #[error("invalid action input: {0}")]
    InvalidInput(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Domain(#[from] hydra_domain::DomainError),
    #[error(transparent)]
    App(#[from] hydra_app::AppError),
    #[error(transparent)]
    Protocol(#[from] hydra_nostr::ProtocolError),
    #[error(transparent)]
    Reddit(#[from] RedditError),
    #[error(transparent)]
    Bridge(#[from] BridgeError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("URL is invalid: {0}")]
    Url(#[from] url::ParseError),
    #[error("HOME is unavailable; set HYDRA_HOME explicitly")]
    MissingHome,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StateEnvelope<'a> {
    schema: &'static str,
    app: &'static str,
    generated_at: String,
    data: HydraState<'a>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HydraState<'a> {
    schema: &'static str,
    durable_root: String,
    storage: StorageView,
    personas: Vec<PersonaView<'a>>,
    drafts: Vec<DraftView<'a>>,
    objects: Vec<ObjectView<'a>>,
    messages: Vec<MessageView>,
    subscriptions: Vec<SubscriptionView>,
    revisits: Vec<RevisitView>,
    reactions: Vec<ReactionView<'a>>,
    follows: Vec<SocialView>,
    public_follow_sets: Vec<PublicFollowSetView<'a>>,
    blocks: Vec<SocialView>,
    filters: Vec<FilterView<'a>>,
    visible_anchors: Vec<String>,
    feed_orders: BTreeMap<&'static str, Vec<String>>,
    my_feed_order: Vec<String>,
    archives: Vec<ArchiveView>,
    operation_count: usize,
    pending_delivery_count: usize,
    reaction_count: usize,
    revisit_count: usize,
    follow_count: usize,
    block_count: usize,
    message_request_count: usize,
    media_count: usize,
    archive_count: usize,
    projections: Vec<ProjectionView<'a>>,
    network_projections: Vec<NetworkProjectionView<'a>>,
    continuity_workflows: Vec<ContinuityWorkflowView>,
    settings: Settings,
    readiness: Vec<ReadinessView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageView {
    root: String,
    media: String,
    media_exists: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ContinuityWorkflowView {
    operation_id: String,
    persona_id: String,
    kind: &'static str,
    state: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadinessView {
    id: &'static str,
    label: &'static str,
    state: &'static str,
    required: bool,
    detail: String,
    next_action: Option<&'static str>,
    last_tested_at: Option<u64>,
    last_success_at: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersonaView<'a> {
    id: String,
    display_name: &'a str,
    public_key: &'a str,
    active: bool,
    reddit_linked: bool,
    reddit_username: Option<String>,
    reddit_proof: Option<&'a str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DraftView<'a> {
    id: &'a str,
    persona_id: String,
    kind: &'static str,
    title: Option<&'a str>,
    body: &'a str,
    communities: Vec<&'a str>,
    parent: Option<&'a str>,
    updated_at: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ObjectView<'a> {
    anchor: &'a str,
    author: &'a str,
    kind: &'static str,
    title: Option<&'a str>,
    body: &'a str,
    communities: Vec<String>,
    root: Option<&'a str>,
    parent: Option<&'a str>,
    external_root: Option<&'a str>,
    external_parent: Option<&'a str>,
    external_source: Option<&'a str>,
    media: Vec<MediaView<'a>>,
    current_score: i64,
    reaffirmations: usize,
    positive_reactions: usize,
    negative_reactions: usize,
    emoji_reactions: BTreeMap<String, usize>,
    unique_voters: usize,
    persistence_score: i64,
    reddit_linked_score: i64,
    trusted_score: i64,
    discussion_count: usize,
    controversy: usize,
    durability: &'static str,
    reddit_projected: bool,
    disowned: bool,
    edited_at: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MediaView<'a> {
    sha256: &'a str,
    mime_type: &'a str,
    size: u64,
    dimensions: Option<&'a str>,
    duration_seconds: Option<u64>,
    original_url: Option<&'a str>,
    blob_urls: &'a [String],
    metadata_event_id: Option<&'a str>,
    preservation: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageView {
    persona_id: String,
    peer: String,
    direction: &'static str,
    body: String,
    created_at: u64,
    request: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubscriptionView {
    persona_id: String,
    community: String,
    public: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RevisitView {
    persona_id: String,
    target: String,
    intent: String,
    due_at: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReactionView<'a> {
    actor: &'a str,
    target: &'a str,
    value: &'a str,
    occurred_at: u64,
    credited_reaffirmation: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SocialView {
    persona_id: String,
    target: String,
    public: bool,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicFollowSetView<'a> {
    persona_id: String,
    identifier: &'a str,
    title: &'a str,
    members: Vec<&'a str>,
    published_at: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilterView<'a> {
    persona_id: String,
    kind: &'static str,
    value: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveView {
    id: String,
    observer_id: String,
    selected: String,
    level: &'static str,
    loaded_count: usize,
    preserved_count: usize,
    media_preserved_count: usize,
    media_unavailable_count: usize,
    captured_at: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectionView<'a> {
    id: String,
    persona_id: String,
    anchor: &'a str,
    destination_system: &'a str,
    destination: &'a str,
    external_id: Option<&'a str>,
    external_url: Option<&'a str>,
    state: &'static str,
    sync_enabled: bool,
    divergence: Option<&'a str>,
    error: Option<&'a str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkProjectionView<'a> {
    author: &'a str,
    anchor: &'a str,
    external_id: &'a str,
    external_url: &'a str,
    reddit_fullname: &'a str,
    community: &'a str,
    projection_type: &'a str,
    state: &'a str,
    recorded_at: u64,
}

#[derive(Debug, Deserialize)]
struct CreatePersonaInput {
    display_name: String,
}

#[derive(Debug, Deserialize)]
struct ImportPersonaInput {
    display_name: String,
    secret: String,
}

#[derive(Debug, Deserialize)]
struct RemotePersonaInput {
    display_name: String,
    bunker_uri: String,
}

#[derive(Debug, Deserialize)]
struct FirefoxInstallInput {
    #[serde(default = "default_true")]
    open: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct SwitchPersonaInput {
    persona_id: String,
}

#[derive(Debug, Deserialize)]
struct UpdatePersonaProfileInput {
    persona_id: String,
    display_name: String,
}

#[derive(Debug, Deserialize)]
struct SaveDraftInput {
    id: Option<String>,
    persona_id: String,
    kind: String,
    title: Option<String>,
    body: String,
    #[serde(default)]
    communities: Vec<String>,
    parent: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DiscardDraftInput {
    persona_id: String,
    id: String,
}

#[derive(Debug, Deserialize)]
struct CreatePostInput {
    persona_id: String,
    title: String,
    body: String,
    communities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CreateCommentInput {
    persona_id: String,
    parent_anchor: String,
    body: String,
}

#[derive(Debug, Deserialize)]
struct CreateExternalCommentInput {
    persona_id: String,
    root_url: Option<String>,
    parent_url: Option<String>,
    root_system: Option<String>,
    root_id: Option<String>,
    parent_system: Option<String>,
    parent_id: Option<String>,
    communities: Vec<String>,
    body: String,
}

#[derive(Debug, Deserialize)]
struct CreateNormInput {
    persona_id: String,
    statement: String,
    community: String,
}

#[derive(Debug, Deserialize)]
struct EditObjectInput {
    persona_id: String,
    anchor: String,
    title: Option<String>,
    body: String,
    communities: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct DisownObjectInput {
    persona_id: String,
    anchor: String,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReactionInput {
    persona_id: String,
    target: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct RevisitInput {
    persona_id: String,
    target: String,
    intent: String,
    due_at: Option<u64>,
    collection: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemoveRevisitInput {
    persona_id: String,
    target: String,
}

#[derive(Debug, Deserialize)]
struct FollowInput {
    persona_id: String,
    target: String,
    public: bool,
    following: bool,
}

#[derive(Debug, Deserialize)]
struct PublishFollowSetInput {
    persona_id: String,
    identifier: String,
    title: String,
    members: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BlockInput {
    persona_id: String,
    target: String,
    public: bool,
    blocked: bool,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LocalFilterInput {
    persona_id: String,
    kind: String,
    value: String,
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct SendMessageInput {
    persona_id: String,
    recipient: String,
    body: String,
    #[serde(default)]
    recipient_relays: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CommunitySubscriptionInput {
    persona_id: String,
    community: String,
    public: bool,
    subscribed: bool,
}

#[derive(Debug, Deserialize)]
struct PreserveMediaInput {
    object: String,
    source_path: String,
    mime_type: String,
    original_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenStorageInput {
    folder: String,
}

#[derive(Debug, Deserialize)]
struct BackupInput {
    persona_id: Option<String>,
    path: String,
    passphrase: String,
}

#[derive(Debug, Deserialize)]
struct SettingsUpdateInput {
    relays: Option<Vec<String>>,
    persona_id: Option<String>,
    persona_read_relays: Option<Vec<String>>,
    persona_write_relays: Option<Vec<String>>,
    inbox_relays: Option<Vec<String>>,
    replication_threshold: Option<usize>,
    theme: Option<String>,
    onboarding_complete: Option<bool>,
    crosspost_default: Option<bool>,
    book_club_cross_links_enabled: Option<bool>,
    persona_crosspost_defaults: Option<BTreeMap<String, bool>>,
    community_crosspost_defaults: Option<BTreeMap<String, bool>>,
    content_crosspost_defaults: Option<BTreeMap<String, bool>>,
    media_copy_enabled: Option<bool>,
    max_media_bytes: Option<u64>,
    persona_blob_servers: Option<BTreeMap<String, Vec<String>>>,
    feed_source_weights: Option<BTreeMap<String, u8>>,
    spam_filter_threshold: Option<u8>,
    remote_media_policy: Option<String>,
    big_stick_enabled: Option<bool>,
    reddacted_enabled: Option<bool>,
    big_stick_archive_level: Option<String>,
    reddacted_archive_level: Option<String>,
    continuity_replication_threshold: Option<usize>,
    preferred_gateway_template: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RedditOAuthBeginInput {
    client_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RedditOAuthCompleteInput {
    persona_id: String,
    client_id: Option<String>,
    expected_state: String,
    callback_state: String,
    code: String,
}

#[derive(Debug, Deserialize)]
struct RedditOAuthConnectInput {
    persona_id: String,
    client_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RedditOAuthUnlinkInput {
    persona_id: String,
}

#[derive(Debug, Deserialize)]
struct RedditIdentityProofInput {
    persona_id: String,
    artifact_url: String,
}

#[derive(Debug, Deserialize)]
struct RedditQueuePostInput {
    persona_id: String,
    anchor: String,
    subreddit: String,
    attribution: Option<String>,
    link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RedditQueueCommentInput {
    persona_id: String,
    anchor: String,
    parent: String,
    attribution: Option<String>,
    link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProjectionInput {
    projection_id: String,
}

#[derive(Debug, Deserialize)]
struct ResolveDuplicatesInput {
    keep_projection_id: String,
}

#[derive(Debug, Deserialize)]
struct ProjectionSyncSettingInput {
    projection_id: String,
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct RedditBrowseCommunityInput {
    persona_id: String,
    subreddit: String,
    #[serde(default = "default_reddit_sort")]
    sort: String,
    after: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RedditBrowseThreadInput {
    persona_id: String,
    post: String,
}

#[derive(Debug, Deserialize)]
struct RedditExportPreviewInput {
    path: String,
}

#[derive(Debug, Deserialize)]
struct RedditExportImportInput {
    persona_id: String,
    path: String,
    #[serde(default)]
    publish: bool,
    #[serde(default)]
    selected: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SearchInput {
    persona_id: Option<String>,
    query: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct OpenNostrInput {
    persona_id: Option<String>,
    since: Option<u64>,
    #[serde(default = "default_open_nostr_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct ResolveNostrInput {
    persona_id: Option<String>,
    uri: String,
}

#[derive(Debug, Deserialize)]
struct NostrCurateInput {
    persona_id: String,
    event_json: String,
    communities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct NostrCategorizeInput {
    persona_id: String,
    event_json: String,
    communities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct KeepNostrInput {
    event_json: String,
}

#[derive(Debug, Deserialize)]
struct RawEventsInput {
    #[serde(default = "default_raw_event_limit")]
    limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LocalSearchQuery {
    Text(String),
    Persona(String),
    Topic(String),
    Reddit(String),
    Provenance(String),
}

impl LocalSearchQuery {
    fn parse(value: &str) -> Result<Self, RuntimeError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(RuntimeError::InvalidInput(
                "search query cannot be empty".to_owned(),
            ));
        }
        let lowered = value.to_lowercase();
        if let Some(value) = lowered.strip_prefix("persona:") {
            return Self::nonempty(value, Self::Persona);
        }
        if let Some(value) = lowered.strip_prefix("topic:") {
            return Self::nonempty(value.trim_start_matches("/h/"), Self::Topic);
        }
        if let Some(value) = lowered.strip_prefix("provenance:") {
            return Self::nonempty(value, Self::Provenance);
        }
        if let Some(value) = lowered.strip_prefix("reddit:") {
            return Self::nonempty(value, Self::Reddit);
        }
        if let Some(value) = lowered.strip_prefix("/h/") {
            return Self::nonempty(value, Self::Topic);
        }
        if lowered.starts_with("https://") && lowered.contains("reddit.com/")
            || lowered.starts_with("t1_")
            || lowered.starts_with("t3_")
        {
            return Ok(Self::Reddit(lowered));
        }
        Ok(Self::Text(lowered))
    }

    fn nonempty(
        value: &str,
        constructor: impl FnOnce(String) -> Self,
    ) -> Result<Self, RuntimeError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(RuntimeError::InvalidInput(
                "structured search value cannot be empty".to_owned(),
            ));
        }
        Ok(constructor(value.to_owned()))
    }
}

#[derive(Debug, Deserialize)]
struct BigStickInput {
    projection_id: String,
    portable_link: Option<String>,
    archive_level: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WithdrawInput {
    projection_id: String,
    portable_link: Option<String>,
    marker: String,
    archive_level: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeBridgeRequest {
    protocol: String,
    kind: String,
    reddit_url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationView {
    id: String,
    status: &'static str,
    progress: u8,
    long_running: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActionResult {
    protocol: &'static str,
    app: &'static str,
    action: String,
    operation: OperationView,
    result: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusEnvelope {
    schema: &'static str,
    app: &'static str,
    generated_at: String,
    #[serde(rename = "state_ready")]
    state_ready: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationStatusEnvelope {
    schema: &'static str,
    app: &'static str,
    generated_at: String,
    operation: OperationView,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DesktopHostRequest {
    command: String,
    action: Option<String>,
    input: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let response = serde_json::json!({
                "schema": "hydra-error/v1",
                "ok": false,
                "error": error.to_string(),
            });
            eprintln!("{response}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), RuntimeError> {
    let mut args = env::args().skip(1);
    let command = args.next().ok_or(RuntimeError::UnknownCommand)?;
    let root = hydra_root()?;
    match command.as_str() {
        "state" => print_state(&root),
        "status" => print_status(&root),
        "action" => {
            let action = args.next().ok_or(RuntimeError::UnknownCommand)?;
            if action.len() > 128
                || !action
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            {
                return Err(RuntimeError::InvalidInput(
                    "action name is invalid".to_owned(),
                ));
            }
            let input = action_input(args.next().ok_or(RuntimeError::MissingInput)?)?;
            run_action(&root, &action, &input).await
        }
        "operation-status" => {
            let operation = args.next().ok_or(RuntimeError::MissingInput)?;
            print_operation_status(&root, &operation)
        }
        "worker-sync" => {
            let operation = args.next().ok_or(RuntimeError::MissingInput)?;
            run_sync_worker(&root, OperationId::parse(&operation)?).await
        }
        "native-host" => run_native_host(&root),
        "desktop-host" => run_desktop_host(&root).await,
        _ => Err(RuntimeError::UnknownCommand),
    }
}

async fn run_desktop_host(root: &PathBuf) -> Result<(), RuntimeError> {
    const MAX_REQUEST: usize = 1_048_576;
    let input = std::io::stdin();
    let mut input = input.lock();
    while let Some(request) = read_desktop_line(&mut input, MAX_REQUEST)? {
        let result = match request {
            DesktopLine::Request(request) => handle_desktop_request(root, &request).await,
            DesktopLine::Invalid(message) => Err(RuntimeError::InvalidInput(message.to_owned())),
        };
        if let Err(error) = result {
            println!(
                "{}",
                serde_json::json!({
                    "schema": "hydra-error/v1",
                    "ok": false,
                    "error": error.to_string(),
                })
            );
        }
        std::io::stdout().flush()?;
    }
    Ok(())
}

enum DesktopLine {
    Request(String),
    Invalid(&'static str),
}

fn read_desktop_line(
    reader: &mut impl BufRead,
    maximum: usize,
) -> Result<Option<DesktopLine>, RuntimeError> {
    let mut bytes = Vec::new();
    let mut oversized = false;
    let mut received = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            if !received {
                return Ok(None);
            }
            break;
        }
        received = true;
        let end = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(buffer.len());
        if !oversized {
            if bytes.len().saturating_add(end) > maximum {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(&buffer[..end]);
            }
        }
        let ended = end < buffer.len();
        reader.consume(end + usize::from(ended));
        if ended {
            break;
        }
    }
    if oversized {
        return Ok(Some(DesktopLine::Invalid("desktop request exceeds 1 MiB")));
    }
    Ok(Some(match String::from_utf8(bytes) {
        Ok(request) => DesktopLine::Request(request),
        Err(_) => DesktopLine::Invalid("desktop request is not UTF-8"),
    }))
}

async fn handle_desktop_request(root: &PathBuf, request: &str) -> Result<(), RuntimeError> {
    let request: DesktopHostRequest = serde_json::from_str(request)?;
    match request.command.as_str() {
        "state" if request.action.is_none() && request.input.is_none() => print_state(root),
        "status" if request.action.is_none() && request.input.is_none() => print_status(root),
        "action" => {
            let action = request.action.ok_or(RuntimeError::UnknownCommand)?;
            if action.is_empty()
                || action.len() > 128
                || !action
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_'))
            {
                return Err(RuntimeError::InvalidInput(
                    "action name is invalid".to_owned(),
                ));
            }
            let input = request.input.ok_or(RuntimeError::MissingInput)?;
            if input.len() > 1_048_576 {
                return Err(RuntimeError::InvalidInput(
                    "action input exceeds 1 MiB".to_owned(),
                ));
            }
            run_action(root, &action, &input).await
        }
        _ => Err(RuntimeError::UnknownCommand),
    }
}

fn action_input(argument: String) -> Result<String, RuntimeError> {
    const MAX_ACTION_INPUT: usize = 1_048_576;
    if argument != "-" {
        if argument.len() > MAX_ACTION_INPUT {
            return Err(RuntimeError::InvalidInput(
                "action input exceeds 1 MiB".to_owned(),
            ));
        }
        return Ok(argument);
    }
    let mut input = String::new();
    std::io::stdin()
        .take(u64::try_from(MAX_ACTION_INPUT + 1).expect("fixed limit fits u64"))
        .read_to_string(&mut input)?;
    if input.len() > MAX_ACTION_INPUT {
        return Err(RuntimeError::InvalidInput(
            "action input exceeds 1 MiB".to_owned(),
        ));
    }
    Ok(input)
}

fn hydra_root() -> Result<PathBuf, RuntimeError> {
    if let Some(root) = env::var_os("HYDRA_HOME") {
        return Ok(PathBuf::from(root));
    }
    env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("hydra"))
        .ok_or(RuntimeError::MissingHome)
}

fn run_native_host(root: &Path) -> Result<(), RuntimeError> {
    const MAX_MESSAGE: usize = 1_048_576;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    loop {
        let mut first = [0_u8; 1];
        if input.read(&mut first)? == 0 {
            return Ok(());
        }
        let mut length_bytes = [0_u8; 4];
        length_bytes[0] = first[0];
        input.read_exact(&mut length_bytes[1..])?;
        let length = usize::try_from(u32::from_le_bytes(length_bytes))
            .map_err(|_| RuntimeError::InvalidInput("native message is too large".to_owned()))?;
        if length == 0 || length > MAX_MESSAGE {
            return Err(RuntimeError::InvalidInput(
                "native message length is invalid".to_owned(),
            ));
        }
        let mut body = vec![0_u8; length];
        input.read_exact(&mut body)?;
        let response = serde_json::from_slice::<NativeBridgeRequest>(&body).map_or_else(
            |error| native_error(&error.to_string()),
            |request| handle_native_request(root, &request),
        );
        let encoded = serde_json::to_vec(&response)?;
        let response_length = u32::try_from(encoded.len())
            .map_err(|_| RuntimeError::InvalidInput("native response is too large".to_owned()))?;
        output.write_all(&response_length.to_le_bytes())?;
        output.write_all(&encoded)?;
        output.flush()?;
    }
}

fn handle_native_request(_root: &Path, request: &NativeBridgeRequest) -> serde_json::Value {
    if request.protocol != "hydra-native-bridge/v1" {
        return native_error("unsupported native bridge protocol");
    }
    if request.kind == "ping" {
        return serde_json::json!({"ok": true, "protocol": "hydra-native-bridge/v1"});
    }
    if request.kind != "open_reddit" {
        return native_error("unsupported native bridge request");
    }
    let Ok(mut reddit) = Url::parse(&request.reddit_url) else {
        return native_error("Reddit URL is invalid");
    };
    if reddit.scheme() != "https"
        || !matches!(reddit.host_str(), Some("www.reddit.com" | "old.reddit.com"))
        || !reddit.username().is_empty()
        || reddit.password().is_some()
        || reddit.port().is_some_and(|port| port != 443)
    {
        return native_error("only HTTPS Reddit URLs are accepted");
    }
    reddit.set_query(None);
    reddit.set_fragment(None);
    let mut deep_link = Url::parse("hydra://reddit").expect("static Hydra deep link is valid");
    deep_link
        .query_pairs_mut()
        .append_pair("url", reddit.as_str())
        .append_pair("source", &request.kind);
    if let Err(error) = launch_external(deep_link.as_str()) {
        return native_error(&error.to_string());
    }
    serde_json::json!({
        "ok": true,
        "protocol": "hydra-native-bridge/v1",
        "deepLink": deep_link.as_str(),
        "opened": request.kind == "open_reddit"
    })
}

fn reddit_fullname_from_url(url: &Url) -> Result<RedditFullname, RuntimeError> {
    let segments = url
        .path_segments()
        .ok_or_else(|| RuntimeError::InvalidInput("Reddit URL has no path".to_owned()))?
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let comments = segments
        .iter()
        .position(|segment| *segment == "comments")
        .ok_or_else(|| {
            RuntimeError::InvalidInput("Reddit URL is not a post or comment".to_owned())
        })?;
    let post = validated_reddit_id(segments.get(comments + 1).copied())?;
    if let Some(comment) = segments.get(comments + 3).copied() {
        return RedditFullname::parse(format!("t1_{}", validated_reddit_id(Some(comment))?))
            .map_err(Into::into);
    }
    RedditFullname::parse(format!("t3_{post}")).map_err(Into::into)
}

fn validated_reddit_id(value: Option<&str>) -> Result<&str, RuntimeError> {
    value
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 32
                && value
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        })
        .ok_or_else(|| RuntimeError::InvalidInput("Reddit object ID is invalid".to_owned()))
}

fn launch_external(target: &str) -> Result<(), RuntimeError> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(target_os = "windows")]
    let mut command = Command::new("rundll32.exe");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    command.args(["url.dll,FileProtocolHandler", target]);
    #[cfg(not(target_os = "windows"))]
    command.arg(target);
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(RuntimeError::InvalidInput(
            "Hydra could not open its desktop deep link".to_owned(),
        ))
    }
}

fn storage_view(root: &Path) -> StorageView {
    let media = root.join("media");
    let media_exists =
        fs::symlink_metadata(&media).is_ok_and(|metadata| metadata.file_type().is_dir());
    StorageView {
        root: root.display().to_string(),
        media: media.display().to_string(),
        media_exists,
    }
}

fn storage_folder(root: &Path, folder: &str) -> Result<PathBuf, RuntimeError> {
    let target = match folder {
        "data" => root.to_path_buf(),
        "media" => root.join("media"),
        _ => {
            return Err(RuntimeError::InvalidInput(
                "storage folder must be data or media".to_owned(),
            ));
        }
    };
    let metadata = fs::symlink_metadata(&target).map_err(|_| {
        RuntimeError::InvalidInput(match folder {
            "media" => "No preserved media folder exists yet".to_owned(),
            _ => "Hydra's local data folder is unavailable".to_owned(),
        })
    })?;
    if !metadata.file_type().is_dir() {
        return Err(RuntimeError::InvalidInput(
            "Hydra will only open a real local data folder".to_owned(),
        ));
    }
    Ok(target)
}

fn launch_folder(target: &Path) -> Result<(), RuntimeError> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(target_os = "windows")]
    let mut command = Command::new("explorer.exe");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = Command::new("xdg-open");
    command.arg(target);
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(RuntimeError::InvalidInput(
            "Hydra could not open the requested local folder".to_owned(),
        ))
    }
}

fn native_error(message: &str) -> serde_json::Value {
    serde_json::json!({
        "ok": false,
        "protocol": "hydra-native-bridge/v1",
        "error": message
    })
}

fn print_state(root: &PathBuf) -> Result<(), RuntimeError> {
    println!("{}", state_envelope(root)?);
    Ok(())
}

fn state_envelope(root: &PathBuf) -> Result<serde_json::Value, RuntimeError> {
    let store = DurableStore::open(root)?;
    let settings = SettingsStore::new(root).load()?;
    let private_states = load_active_private_state(&store, &settings)?;
    let personas = persona_views(&store, &settings);
    let objects = object_views(&store, &settings, &private_states);
    let drafts = draft_views(&private_states);
    let messages = message_views(&private_states);
    let message_request_count = messages.iter().filter(|message| message.request).count();
    let subscriptions = subscription_views(&store, &private_states);
    let revisits = revisit_views(&private_states);
    let reactions = reaction_views(&store);
    let follows = follow_views(&store, &private_states);
    let public_follow_sets = public_follow_set_views(&store);
    let blocks = block_views(&store, &private_states);
    let filters = filter_views(&private_states);
    let (visible_anchors, feed_orders, my_feed_order) =
        lens_views(&store, &settings, &private_states);
    let archives = archive_views(&store);
    let projections = projection_views(&store);
    let continuity_workflows = continuity_workflow_views(&store);
    let readiness = readiness_views(root, &store, &settings);
    let data = HydraState {
        schema: "hydra-state/v1",
        durable_root: root.display().to_string(),
        storage: storage_view(root),
        personas,
        drafts,
        objects,
        messages,
        subscriptions,
        revisits,
        reactions,
        follows,
        public_follow_sets,
        blocks,
        filters,
        visible_anchors,
        feed_orders,
        my_feed_order,
        archives,
        operation_count: store.state().operations.len(),
        pending_delivery_count: store.state().pending_delivery_count(),
        reaction_count: store.state().reactions.len(),
        revisit_count: private_states
            .iter()
            .map(|state| state.revisits.len())
            .sum(),
        follow_count: store
            .state()
            .follows
            .values()
            .filter(|follow| follow.following)
            .count()
            + private_states
                .iter()
                .flat_map(|state| state.follows.values())
                .filter(|follow| follow.following)
                .count(),
        block_count: store
            .state()
            .blocks
            .values()
            .filter(|block| block.blocked)
            .count()
            + private_states
                .iter()
                .flat_map(|state| state.blocks.values())
                .filter(|block| block.blocked)
                .count(),
        message_request_count,
        media_count: store.state().media.len(),
        archive_count: store.state().archive_manifests.len(),
        projections,
        network_projections: network_projection_views(&store),
        continuity_workflows,
        settings,
        readiness,
    };
    Ok(serde_json::to_value(StateEnvelope {
        schema: "theurgy-state-snapshot/v1",
        app: "hydra",
        generated_at: generated_at(),
        data,
    })?)
}

fn continuity_workflow_views(store: &DurableStore) -> Vec<ContinuityWorkflowView> {
    store
        .state()
        .continuity_workflows
        .values()
        .map(|workflow| {
            let (kind, state) = match workflow.state {
                ContinuityState::BigStick(state) => ("big_stick", format!("{state:?}")),
                ContinuityState::Reddacted(state) => ("reddacted", format!("{state:?}")),
                ContinuityState::PersonaSwitch(state) => ("persona_switch", format!("{state:?}")),
            };
            ContinuityWorkflowView {
                operation_id: workflow.id.to_string(),
                persona_id: workflow.persona.to_string(),
                kind,
                state: state.to_lowercase(),
            }
        })
        .collect()
}

fn lens_views(
    store: &DurableStore,
    settings: &Settings,
    private_states: &[PrivateState],
) -> (
    Vec<String>,
    BTreeMap<&'static str, Vec<String>>,
    Vec<String>,
) {
    let Some(persona) = active_persona_id(store, settings) else {
        let visible_anchors = store
            .state()
            .heads
            .current_heads()
            .map(|head| head.anchor.as_str().to_owned())
            .collect();
        let feed_orders = FeedLens::ALL
            .into_iter()
            .map(|lens| {
                (
                    lens.as_str(),
                    FeedService::public_feed(store, lens)
                        .into_iter()
                        .map(|head| head.anchor.as_str().to_owned())
                        .collect(),
                )
            })
            .collect();
        return (visible_anchors, feed_orders, Vec::new());
    };
    let Some(private) = private_states.first() else {
        return (Vec::new(), BTreeMap::new(), Vec::new());
    };
    let visible_anchors = store
        .state()
        .heads
        .current_heads()
        .filter(|head| FeedService::visible(store, private, persona, settings, head))
        .map(|head| head.anchor.as_str().to_owned())
        .collect();
    let feed_orders = FeedLens::ALL
        .into_iter()
        .map(|lens| {
            (
                lens.as_str(),
                FeedService::feed(store, private, persona, settings, lens)
                    .into_iter()
                    .map(|head| head.anchor.as_str().to_owned())
                    .collect(),
            )
        })
        .collect();
    let my_feed_order = FeedService::my_feed(store, private, persona, settings)
        .into_iter()
        .map(|head| head.anchor.as_str().to_owned())
        .collect();
    (visible_anchors, feed_orders, my_feed_order)
}

fn persona_views<'a>(store: &'a DurableStore, settings: &Settings) -> Vec<PersonaView<'a>> {
    store
        .state()
        .personas
        .iter()
        .map(|persona| PersonaView {
            id: persona.id.to_string(),
            display_name: &persona.display_name,
            public_key: persona.public_key.as_str(),
            active: settings.active_persona_id.as_deref() == Some(&persona.id.to_string()),
            reddit_linked: persona.reddit_account.is_some(),
            reddit_username: persona.reddit_account.as_ref().and_then(|_| {
                PlatformRedditCredentialStore
                    .get(persona.id)
                    .ok()
                    .map(|credential| credential.identity.username)
            }),
            reddit_proof: store
                .state()
                .reddit_identity_proofs
                .get(&persona.id)
                .map(|proof| proof.artifact_url.as_str()),
        })
        .collect()
}

fn object_views<'a>(
    store: &'a DurableStore,
    settings: &Settings,
    private_states: &[PrivateState],
) -> Vec<ObjectView<'a>> {
    let trusted = trusted_actors(store, settings, private_states);
    let local_topics = settings
        .active_persona_id
        .as_deref()
        .and_then(|persona| settings.local_topic_assignments.get(persona));
    store
        .state()
        .heads
        .current_heads()
        .map(|head| object_view(store, settings, local_topics, &trusted, head))
        .collect()
}

fn object_view<'a>(
    store: &'a DurableStore,
    settings: &Settings,
    local_topics: Option<&BTreeMap<String, Vec<String>>>,
    trusted: &BTreeSet<&NostrPublicKey>,
    head: &'a hydra_domain::ObjectHead,
) -> ObjectView<'a> {
    ObjectView {
        anchor: head.anchor.as_str(),
        author: head.author.as_str(),
        kind: match head.kind {
            hydra_domain::ObjectKind::Post => "post",
            hydra_domain::ObjectKind::Comment => "comment",
            hydra_domain::ObjectKind::Norm => "norm",
        },
        title: head.title.as_deref(),
        body: head.body.as_str(),
        communities: object_communities(head, local_topics),
        root: head.root.as_ref().map(AnchorId::as_str),
        parent: head.parent.as_ref().map(AnchorId::as_str),
        external_root: head
            .external_root
            .as_ref()
            .map(|item| item.canonical.as_str()),
        external_parent: head
            .external_parent
            .as_ref()
            .map(|item| item.canonical.as_str()),
        external_source: head
            .external_source
            .as_ref()
            .map(|item| item.canonical.as_str()),
        media: object_media(store, &head.anchor),
        current_score: runtime_current_score(store, &head.anchor),
        reaffirmations: store
            .state()
            .reactions
            .iter()
            .filter(|reaction| reaction.target == head.anchor && reaction.credited_reaffirmation)
            .count(),
        positive_reactions: reaction_count(store, &head.anchor, &ReactionValue::Upvote),
        negative_reactions: reaction_count(store, &head.anchor, &ReactionValue::Downvote),
        emoji_reactions: emoji_reaction_counts(store, &head.anchor),
        unique_voters: reaction_actors(store, &head.anchor).len(),
        persistence_score: persistence_score(store, &head.anchor),
        reddit_linked_score: reddit_linked_score(store, &head.anchor),
        trusted_score: score_for_actors(store, &head.anchor, trusted),
        discussion_count: store
            .state()
            .heads
            .current_heads()
            .filter(|candidate| candidate.root.as_ref() == Some(&head.anchor))
            .count(),
        controversy: runtime_controversy(store, &head.anchor),
        durability: durability_state(
            store,
            &head.anchor,
            &head.author,
            settings.replication_threshold,
        ),
        reddit_projected: store.state().projections.values().any(|projection| {
            projection.anchor == head.anchor
                && projection.external_id.is_some()
                && projection.state != hydra_domain::ProjectionState::Withdrawn
        }) || store
            .state()
            .public_projections
            .values()
            .any(|projection| projection.anchor == head.anchor && projection.state != "withdrawn"),
        disowned: store
            .state()
            .disowning_requests
            .keys()
            .any(|(_, anchor)| anchor == &head.anchor),
        edited_at: head.edited_at,
    }
}

fn object_communities(
    head: &hydra_domain::ObjectHead,
    local_topics: Option<&BTreeMap<String, Vec<String>>>,
) -> Vec<String> {
    head.communities
        .iter()
        .map(|community| community.as_str().to_owned())
        .chain(
            local_topics
                .and_then(|assignments| assignments.get(head.anchor.as_str()))
                .into_iter()
                .flatten()
                .cloned(),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn object_media<'a>(store: &'a DurableStore, anchor: &AnchorId) -> Vec<MediaView<'a>> {
    store
        .state()
        .media
        .values()
        .filter(|manifest| manifest.object == *anchor)
        .map(|manifest| MediaView {
            sha256: &manifest.sha256,
            mime_type: &manifest.mime_type,
            size: manifest.size,
            dimensions: manifest.dimensions.as_deref(),
            duration_seconds: manifest.duration_seconds,
            original_url: manifest.original_url.as_deref(),
            blob_urls: &manifest.blob_urls,
            metadata_event_id: manifest.metadata_event_id.as_deref(),
            preservation: if manifest.blob_urls.is_empty() {
                "local_only"
            } else if manifest.metadata_event_id.is_none() {
                "media_only"
            } else {
                "published"
            },
        })
        .collect()
}

fn draft_views(private_states: &[PrivateState]) -> Vec<DraftView<'_>> {
    private_states
        .iter()
        .flat_map(|state| state.drafts.values())
        .map(|draft| DraftView {
            id: &draft.id,
            persona_id: draft.persona.to_string(),
            kind: match draft.kind {
                DraftKind::Post => "post",
                DraftKind::Comment => "comment",
                DraftKind::Norm => "norm",
            },
            title: draft.title.as_deref(),
            body: &draft.body,
            communities: draft.communities.iter().map(CommunityKey::as_str).collect(),
            parent: draft.parent.as_ref().map(AnchorId::as_str),
            updated_at: draft.updated_at,
        })
        .collect()
}

fn readiness_views(root: &Path, store: &DurableStore, settings: &Settings) -> Vec<ReadinessView> {
    let has_personas = store.state().personas.iter().next().is_some();
    let active_persona = active_persona_id(store, settings);
    let reddit_linked = active_persona
        .and_then(|persona| store.state().personas.get(persona))
        .is_some_and(|persona| persona.reddit_account.is_some());
    vec![
        ReadinessView {
            id: "local-storage",
            label: "Local storage",
            state: "ready",
            required: true,
            detail: local_storage_detail(root, active_persona),
            next_action: None,
            last_tested_at: Some(unix_now()),
            last_success_at: Some(unix_now()),
        },
        ReadinessView {
            id: "identity-backup",
            label: "Identity backup",
            state: if !has_personas || settings.last_backup_at.is_some() {
                "ready"
            } else {
                "missing"
            },
            required: false,
            detail: settings.last_backup_at.map_or_else(
                || "No verified encrypted backup yet".to_owned(),
                |timestamp| format!("Last exported at unix:{timestamp}"),
            ),
            next_action: (has_personas && settings.last_backup_at.is_none())
                .then_some("backup.export"),
            last_tested_at: settings.last_backup_at,
            last_success_at: settings.last_backup_at,
        },
        ReadinessView {
            id: "nostr-relays",
            label: "Nostr relays",
            state: probe_state(&settings.relay_probe, true),
            required: true,
            detail: if settings.relay_probe.detail.is_empty() {
                format!(
                    "Not tested; {} configured and {} acknowledgements required",
                    settings.relays.len(),
                    settings.replication_threshold
                )
            } else {
                settings.relay_probe.detail.clone()
            },
            next_action: Some("readiness.probe"),
            last_tested_at: settings.relay_probe.last_tested_at,
            last_success_at: settings.relay_probe.last_success_at,
        },
        ReadinessView {
            id: "reddit-bridge",
            label: "Reddit Bridge",
            state: if reddit_linked {
                probe_state(&settings.reddit_probe, false)
            } else {
                "missing"
            },
            required: false,
            detail: reddit_readiness_detail(settings, active_persona, reddit_linked),
            next_action: Some(if reddit_linked {
                "readiness.probe"
            } else {
                "reddit.oauth.connect"
            }),
            last_tested_at: settings.reddit_probe.last_tested_at,
            last_success_at: settings.reddit_probe.last_success_at,
        },
    ]
}

fn local_storage_detail(root: &Path, active_persona: Option<PersonaId>) -> String {
    active_persona.map_or_else(
        || root.display().to_string(),
        |persona| {
            format!(
                "{} · identity: {}",
                root.display(),
                PlatformSecretStore::custody_label(persona)
            )
        },
    )
}

fn reddit_readiness_detail(
    settings: &Settings,
    active_persona: Option<PersonaId>,
    reddit_linked: bool,
) -> String {
    if !reddit_linked {
        return "Optional; Hydra works without Reddit".to_owned();
    }
    let probe = if settings.reddit_probe.detail.is_empty() {
        "Linked but not tested"
    } else {
        &settings.reddit_probe.detail
    };
    active_persona.map_or_else(
        || probe.to_owned(),
        |persona| {
            format!(
                "{probe} · credential: {}",
                PlatformRedditCredentialStore::custody_label(persona)
            )
        },
    )
}

fn probe_state(probe: &ReadinessProbe, required: bool) -> &'static str {
    if probe.last_tested_at.is_none() {
        "untested"
    } else if probe.ready {
        "ready"
    } else if required {
        "failed"
    } else {
        "degraded"
    }
}

fn projection_views(store: &DurableStore) -> Vec<ProjectionView<'_>> {
    store
        .state()
        .projections
        .values()
        .map(|projection| ProjectionView {
            id: projection.id.to_string(),
            persona_id: projection.persona.to_string(),
            anchor: projection.anchor.as_str(),
            destination_system: &projection.destination.system,
            destination: &projection.destination.canonical,
            external_id: projection
                .external_id
                .as_ref()
                .map(|external| external.canonical.as_str()),
            external_url: projection.external_url.as_deref(),
            state: projection_state_name(projection.state),
            sync_enabled: projection.sync_enabled,
            divergence: projection.divergence.as_deref(),
            error: projection.display_error.as_deref(),
        })
        .collect()
}

fn network_projection_views(store: &DurableStore) -> Vec<NetworkProjectionView<'_>> {
    store
        .state()
        .public_projections
        .values()
        .map(|projection| NetworkProjectionView {
            author: projection.author.as_str(),
            anchor: projection.anchor.as_str(),
            external_id: &projection.external_id.canonical,
            external_url: &projection.external_url,
            reddit_fullname: &projection.reddit_fullname,
            community: &projection.target_subreddit,
            projection_type: &projection.projection_type,
            state: &projection.state,
            recorded_at: projection.recorded_at,
        })
        .collect()
}

fn projection_state_name(state: hydra_domain::ProjectionState) -> &'static str {
    match state {
        hydra_domain::ProjectionState::NotRequested => "not_requested",
        hydra_domain::ProjectionState::Queued => "queued",
        hydra_domain::ProjectionState::Submitting => "submitting",
        hydra_domain::ProjectionState::Live => "live",
        hydra_domain::ProjectionState::Synchronizing => "synchronizing",
        hydra_domain::ProjectionState::Diverged => "diverged",
        hydra_domain::ProjectionState::Locked => "locked",
        hydra_domain::ProjectionState::Removed => "removed",
        hydra_domain::ProjectionState::Deleted => "deleted",
        hydra_domain::ProjectionState::Rejected => "rejected",
        hydra_domain::ProjectionState::Withdrawn => "withdrawn",
        hydra_domain::ProjectionState::Failed => "failed",
        hydra_domain::ProjectionState::Abandoned => "abandoned",
    }
}

fn runtime_current_score(store: &DurableStore, target: &AnchorId) -> i64 {
    reaction_actors(store, target)
        .into_iter()
        .filter_map(|actor| store.state().current_stance(actor, target))
        .map(|stance| match stance {
            ReactionValue::Upvote => 1,
            ReactionValue::Downvote => -1,
            ReactionValue::Neutral | ReactionValue::Emoji(_) => 0,
        })
        .sum()
}

fn reaction_actors<'a>(store: &'a DurableStore, target: &AnchorId) -> BTreeSet<&'a NostrPublicKey> {
    store
        .state()
        .reactions
        .iter()
        .filter(|reaction| reaction.target == *target)
        .map(|reaction| &reaction.actor)
        .collect()
}

fn reaction_count(store: &DurableStore, target: &AnchorId, value: &ReactionValue) -> usize {
    store
        .state()
        .reactions
        .iter()
        .filter(|reaction| reaction.target == *target && reaction.value == *value)
        .count()
}

fn emoji_reaction_counts(store: &DurableStore, target: &AnchorId) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for reaction in &store.state().reactions {
        if reaction.target != *target {
            continue;
        }
        if let ReactionValue::Emoji(value) = &reaction.value {
            *counts.entry(value.clone()).or_insert(0) += 1;
        }
    }
    counts
}

fn persistence_score(store: &DurableStore, target: &AnchorId) -> i64 {
    runtime_current_score(store, target)
        + store
            .state()
            .reactions
            .iter()
            .filter(|reaction| reaction.target == *target && reaction.credited_reaffirmation)
            .map(|reaction| match reaction.value {
                ReactionValue::Upvote => 1,
                ReactionValue::Downvote => -1,
                ReactionValue::Neutral | ReactionValue::Emoji(_) => 0,
            })
            .sum::<i64>()
}

fn reddit_linked_score(store: &DurableStore, target: &AnchorId) -> i64 {
    let linked = store
        .state()
        .personas
        .iter()
        .filter(|persona| persona.reddit_account.is_some())
        .map(|persona| &persona.public_key)
        .collect::<BTreeSet<_>>();
    score_for_actors(store, target, &linked)
}

fn score_for_actors(
    store: &DurableStore,
    target: &AnchorId,
    allowed: &BTreeSet<&NostrPublicKey>,
) -> i64 {
    reaction_actors(store, target)
        .into_iter()
        .filter(|actor| allowed.contains(actor))
        .filter_map(|actor| store.state().current_stance(actor, target))
        .map(|stance| match stance {
            ReactionValue::Upvote => 1,
            ReactionValue::Downvote => -1,
            ReactionValue::Neutral | ReactionValue::Emoji(_) => 0,
        })
        .sum()
}

fn trusted_actors<'a>(
    store: &'a DurableStore,
    settings: &Settings,
    private_states: &'a [PrivateState],
) -> BTreeSet<&'a NostrPublicKey> {
    let Some(active) = settings
        .active_persona_id
        .as_deref()
        .and_then(|value| PersonaId::parse(value).ok())
    else {
        return BTreeSet::new();
    };
    let mut trusted = store
        .state()
        .follows
        .values()
        .filter(|follow| follow.persona == active && follow.following)
        .map(|follow| &follow.target)
        .collect::<BTreeSet<_>>();
    trusted.extend(private_states.iter().flat_map(|state| {
        state
            .follows
            .values()
            .filter(move |follow| follow.persona == active && follow.following)
            .map(|follow| &follow.target)
    }));
    if let Some(persona) = store.state().personas.get(active) {
        trusted.insert(&persona.public_key);
    }
    trusted
}

fn runtime_controversy(store: &DurableStore, target: &AnchorId) -> usize {
    let up = store
        .state()
        .reactions
        .iter()
        .filter(|reaction| reaction.target == *target && reaction.value == ReactionValue::Upvote)
        .count();
    let down = store
        .state()
        .reactions
        .iter()
        .filter(|reaction| reaction.target == *target && reaction.value == ReactionValue::Downvote)
        .count();
    up.min(down)
}

fn durability_state(
    store: &DurableStore,
    target: &AnchorId,
    author: &NostrPublicKey,
    replication_threshold: usize,
) -> &'static str {
    let outbound = store.state().outbound.values().filter_map(|event| {
        let value = serde_json::from_str::<serde_json::Value>(&event.event_json).ok()?;
        let matches = value.get("tags")?.as_array()?.iter().any(|tag| {
            tag.as_array().is_some_and(|parts| {
                parts.first().and_then(serde_json::Value::as_str) == Some("d")
                    && parts
                        .get(1)
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|value| value.ends_with(target.as_str()))
            })
        });
        let created_at = value
            .get("created_at")
            .and_then(serde_json::Value::as_u64)?;
        matches.then_some((event, created_at))
    });
    let Some((latest, _)) = outbound.max_by_key(|(_, created_at)| *created_at) else {
        return if store
            .state()
            .personas
            .iter()
            .any(|persona| persona.public_key == *author)
        {
            "local_only"
        } else {
            "published"
        };
    };
    let accepted = latest
        .relays
        .iter()
        .filter(|relay| {
            matches!(
                store
                    .state()
                    .deliveries
                    .get(&(latest.event_id.clone(), (*relay).clone())),
                Some(hydra_domain::DeliveryState::Accepted)
            )
        })
        .count();
    if accepted >= replication_threshold {
        "replicated"
    } else if accepted > 0 {
        "published"
    } else {
        "queued"
    }
}

fn active_persona_id(store: &DurableStore, settings: &Settings) -> Option<PersonaId> {
    settings
        .active_persona_id
        .as_deref()
        .and_then(|value| PersonaId::parse(value).ok())
        .filter(|persona| store.state().personas.get(*persona).is_some())
}

fn load_active_private_state(
    store: &DurableStore,
    settings: &Settings,
) -> Result<Vec<PrivateState>, RuntimeError> {
    active_persona_id(store, settings)
        .map(|persona| private_state(&PlatformSecretStore, store, persona).map_err(Into::into))
        .transpose()
        .map(|state| state.into_iter().collect())
}

fn allowed_read_relays(
    settings: &Settings,
    store: &DurableStore,
    persona: PersonaId,
) -> Result<Vec<String>, RuntimeError> {
    let blocked = private_state(&PlatformSecretStore, store, persona)?
        .filters
        .values()
        .filter(|filter| filter.kind == LocalFilterKind::Relay && filter.enabled)
        .map(|filter| filter.value.trim_end_matches('/').to_lowercase())
        .collect::<BTreeSet<_>>();
    Ok(settings
        .read_relays_for(persona)
        .iter()
        .filter(|relay| !blocked.contains(&relay.trim_end_matches('/').to_lowercase()))
        .cloned()
        .collect())
}

fn message_views(states: &[PrivateState]) -> Vec<MessageView> {
    states
        .iter()
        .flat_map(|state| state.messages.iter())
        .map(|message| MessageView {
            persona_id: message.persona.to_string(),
            peer: message.peer.to_string(),
            direction: match message.direction {
                MessageDirection::Sent => "sent",
                MessageDirection::Received => "received",
            },
            body: message.body.clone(),
            created_at: message.created_at,
            request: message.request,
        })
        .collect()
}

fn subscription_views(store: &DurableStore, private: &[PrivateState]) -> Vec<SubscriptionView> {
    let mut views = store
        .state()
        .subscriptions
        .values()
        .filter(|item| item.subscribed)
        .map(|item| SubscriptionView {
            persona_id: item.persona.to_string(),
            community: item.community.as_str().to_owned(),
            public: true,
        })
        .collect::<Vec<_>>();
    views.extend(private.iter().flat_map(|state| {
        state
            .subscriptions
            .values()
            .filter(|item| item.subscribed)
            .map(|item| SubscriptionView {
                persona_id: item.persona.to_string(),
                community: item.community.as_str().to_owned(),
                public: false,
            })
    }));
    views
}

fn revisit_views(private: &[PrivateState]) -> Vec<RevisitView> {
    private
        .iter()
        .flat_map(|state| state.revisits.values())
        .map(|item| RevisitView {
            persona_id: item.persona.to_string(),
            target: item.target.as_str().to_owned(),
            intent: match &item.intent {
                RevisitIntent::ReturnSoon => "return_soon".to_owned(),
                RevisitIntent::ReconsiderVote => "reconsider_vote".to_owned(),
                RevisitIntent::ReviewOnDate => "review_on_date".to_owned(),
                RevisitIntent::Study => "study".to_owned(),
                RevisitIntent::NotifyOnActivity => "notify_on_activity".to_owned(),
                RevisitIntent::Collection(name) => format!("collection:{name}"),
            },
            due_at: item.due_at,
        })
        .collect()
}

fn reaction_views(store: &DurableStore) -> Vec<ReactionView<'_>> {
    store
        .state()
        .reactions
        .iter()
        .filter_map(|reaction| {
            Some(ReactionView {
                actor: reaction.actor.as_str(),
                target: reaction.target.as_str(),
                value: reaction.value.wire_value().ok()?,
                occurred_at: reaction.occurred_at,
                credited_reaffirmation: reaction.credited_reaffirmation,
            })
        })
        .collect()
}

fn follow_views(store: &DurableStore, private: &[PrivateState]) -> Vec<SocialView> {
    let mut views = store
        .state()
        .follows
        .values()
        .filter(|item| item.following)
        .map(|item| SocialView {
            persona_id: item.persona.to_string(),
            target: item.target.to_string(),
            public: true,
            reason: None,
        })
        .collect::<Vec<_>>();
    views.extend(private.iter().flat_map(|state| {
        state
            .follows
            .values()
            .filter(|item| item.following)
            .map(|item| SocialView {
                persona_id: item.persona.to_string(),
                target: item.target.to_string(),
                public: false,
                reason: None,
            })
    }));
    views
}

fn public_follow_set_views(store: &DurableStore) -> Vec<PublicFollowSetView<'_>> {
    store
        .state()
        .public_follow_sets
        .values()
        .map(|set| PublicFollowSetView {
            persona_id: set.persona.to_string(),
            identifier: &set.identifier,
            title: &set.title,
            members: set.members.iter().map(NostrPublicKey::as_str).collect(),
            published_at: set.published_at,
        })
        .collect()
}

fn block_views(store: &DurableStore, private: &[PrivateState]) -> Vec<SocialView> {
    let mut views = store
        .state()
        .blocks
        .values()
        .filter(|item| item.blocked)
        .map(|item| SocialView {
            persona_id: item.persona.to_string(),
            target: item.target.to_string(),
            public: true,
            reason: item.reason.clone(),
        })
        .collect::<Vec<_>>();
    views.extend(private.iter().flat_map(|state| {
        state
            .blocks
            .values()
            .filter(|item| item.blocked)
            .map(|item| SocialView {
                persona_id: item.persona.to_string(),
                target: item.target.to_string(),
                public: false,
                reason: item.reason.clone(),
            })
    }));
    views
}

fn filter_views(private: &[PrivateState]) -> Vec<FilterView<'_>> {
    private
        .iter()
        .flat_map(|state| state.filters.values())
        .map(|item| FilterView {
            persona_id: item.persona.to_string(),
            kind: match item.kind {
                LocalFilterKind::Word => "word",
                LocalFilterKind::Topic => "topic",
                LocalFilterKind::Thread => "thread",
                LocalFilterKind::Media => "media",
                LocalFilterKind::Relay => "relay",
            },
            value: &item.value,
        })
        .collect()
}

fn archive_views(store: &DurableStore) -> Vec<ArchiveView> {
    store
        .state()
        .archive_manifests
        .values()
        .map(|item| ArchiveView {
            id: item.id.to_string(),
            observer_id: item.observer.to_string(),
            selected: item.selected.canonical.clone(),
            level: match item.level {
                PreservationLevel::Item => "item",
                PreservationLevel::Ancestors => "ancestors",
                PreservationLevel::VisibleSiblings => "visible_siblings",
                PreservationLevel::LoadedThread => "loaded_thread",
            },
            loaded_count: item.loaded.len(),
            preserved_count: item.preserved.len(),
            media_preserved_count: item.media_preserved.len(),
            media_unavailable_count: item.media_unavailable.len(),
            captured_at: item.captured_at,
        })
        .collect()
}

fn print_status(root: &PathBuf) -> Result<(), RuntimeError> {
    DurableStore::open(root)?;
    println!(
        "{}",
        serde_json::to_string(&StatusEnvelope {
            schema: "theurgy-runtime-status/v1",
            app: "hydra",
            generated_at: generated_at(),
            state_ready: true,
        })?
    );
    Ok(())
}

fn print_operation_status(root: &PathBuf, operation: &str) -> Result<(), RuntimeError> {
    let operation = OperationId::parse(operation)?;
    let store = DurableStore::open(root)?;
    let state = store
        .state()
        .operations
        .get(&operation)
        .copied()
        .ok_or_else(|| RuntimeError::InvalidInput("operation not found".to_owned()))?;
    println!(
        "{}",
        serde_json::to_string(&OperationStatusEnvelope {
            schema: "theurgy-operation-status/v1",
            app: "hydra",
            generated_at: generated_at(),
            operation: operation_view(operation, state, true),
        })?
    );
    Ok(())
}

async fn run_action(root: &PathBuf, action: &str, input: &str) -> Result<(), RuntimeError> {
    match action {
        "refresh_state" => refresh_action(root),
        "persona.create" => create_persona_action(root, input),
        "persona.import" => import_persona_action(root, input),
        "persona.connect_remote" => connect_remote_persona_action(root, input).await,
        "persona.switch" => switch_persona_action(root, input),
        "persona.profile.update" => update_persona_profile_action(root, input),
        "draft.save" => save_draft_action(root, input),
        "draft.discard" => discard_draft_action(root, input),
        "post.create" => create_post_action(root, input),
        "comment.create" => create_comment_action(root, input),
        "comment.create_external" => create_external_comment_action(root, input),
        "norm.create" => create_norm_action(root, input),
        "object.edit" => edit_object_action(root, input),
        "object.disown" => disown_object_action(root, input),
        "reaction.set" => reaction_action(root, input),
        "revisit.set" => revisit_action(root, input),
        "revisit.remove" => remove_revisit_action(root, input),
        "follow.set" => follow_action(root, input),
        "follow_set.publish" => publish_follow_set_action(root, input),
        "block.set" => block_action(root, input),
        "filter.set" => local_filter_action(root, input),
        "message.send" => send_message_action(root, input).await,
        "community.subscribe" => community_subscription_action(root, input),
        "media.preserve" => preserve_media_action(root, input),
        "storage.open" => storage_open_action(root, input),
        "backup.export" => backup_export_action(root, input),
        "backup.restore" => backup_restore_action(root, input),
        "settings.update" => settings_update_action(root, input),
        "firefox.install" => firefox_install_action(input),
        "readiness.probe" => readiness_probe_action(root).await,
        "reddit.oauth.begin" => reddit_oauth_begin_action(input),
        "reddit.oauth.complete" => reddit_oauth_complete_action(root, input),
        "reddit.oauth.connect" => reddit_oauth_connect_action(root, input),
        "reddit.oauth.unlink" => reddit_oauth_unlink_action(root, input),
        "reddit.identity_proof.publish" => reddit_identity_proof_action(root, input),
        "reddit.browse.community" => reddit_browse_community_action(input),
        "reddit.browse.thread" => reddit_browse_thread_action(input),
        "reddit.export.preview" => reddit_export_preview_action(input),
        "reddit.export.import" => reddit_export_import_action(root, input),
        "reddit.post.queue" => reddit_queue_post_action(root, input),
        "reddit.comment.queue" => reddit_queue_comment_action(root, input),
        "reddit.projection.execute" => reddit_execute_action(root, input),
        "reddit.projection.sync" => reddit_projection_sync_action(root, input),
        "reddit.projection.sync_setting" => reddit_projection_sync_setting_action(root, input),
        "reddit.projection.resolve_duplicates" => {
            reddit_projection_resolve_duplicates_action(root, input)
        }
        "reddit.divergence.adopt" => reddit_divergence_adopt_action(root, input),
        "reddit.divergence.restore" => reddit_divergence_restore_action(root, input),
        "reddit.divergence.keep" => reddit_divergence_keep_action(root, input),
        "reddit.big_stick" => reddit_big_stick_action(root, input),
        "reddit.withdraw" => reddit_withdraw_action(root, input),
        "search.local" => search_local_action(root, input),
        "search.network" => search_network_action(root, input).await,
        "nostr.open" => open_nostr_action(root, input).await,
        "nostr.resolve" => resolve_nostr_action(root, input).await,
        "nostr.curate" => curate_nostr_action(root, input),
        "nostr.categorize_local" => categorize_nostr_action(root, input),
        "nostr.keep" => keep_nostr_action(root, input),
        "events.raw" => raw_events_action(root, input),
        "sync.now" => sync_action(root, input),
        other => Err(RuntimeError::UnknownAction(other.to_owned())),
    }
}

fn storage_open_action(root: &Path, input: &str) -> Result<(), RuntimeError> {
    let input: OpenStorageInput = serde_json::from_str(input)?;
    let target = storage_folder(root, &input.folder)?;
    launch_folder(&target)?;
    print_action_result(
        "storage.open",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": false, "opened": input.folder}),
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn firefox_install_action(input: &str) -> Result<(), RuntimeError> {
    let input: FirefoxInstallInput = serde_json::from_str(input)?;
    let executable = env::current_exe()?;
    let directory = executable.parent().ok_or_else(|| {
        RuntimeError::InvalidInput("Hydra runtime has no installation directory".to_owned())
    })?;
    let host = directory.join("hydra-native-host");
    if !host.is_file() {
        return Err(RuntimeError::InvalidInput(
            "the packaged Firefox native host is missing".to_owned(),
        ));
    }
    let home = env::var_os("HOME").ok_or(RuntimeError::MissingHome)?;
    #[cfg(target_os = "macos")]
    let destination =
        PathBuf::from(&home).join("Library/Application Support/Mozilla/NativeMessagingHosts");
    #[cfg(target_os = "linux")]
    let destination = PathBuf::from(&home).join(".mozilla/native-messaging-hosts");
    fs::create_dir_all(&destination)?;
    let manifest = serde_json::json!({
        "name": "org.hydra.desktop",
        "description": "Narrow native bridge to the Hydra desktop app",
        "path": host.canonicalize()?.to_string_lossy(),
        "type": "stdio",
        "allowed_extensions": ["hydra-companion@hydra.local"]
    });
    let manifest_path = destination.join("org.hydra.desktop.json");
    let mut temporary = tempfile::NamedTempFile::new_in(&destination)?;
    temporary.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&manifest_path)
        .map_err(|error| RuntimeError::Io(error.error))?;
    #[cfg(unix)]
    fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o600))?;

    #[cfg(target_os = "macos")]
    let xpi = directory
        .parent()
        .map(|resources| resources.join("firefox/hydra-companion.xpi"));
    #[cfg(target_os = "linux")]
    let xpi = Some(PathBuf::from(
        "/usr/share/hydra/firefox/hydra-companion.xpi",
    ));
    let xpi = xpi.filter(|path| path.is_file());
    let opened = input.open
        && xpi
            .as_ref()
            .is_some_and(|path| open_firefox_extension(path));
    print_action_result(
        "firefox.install",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({
            "nativeHostManifest": manifest_path,
            "extensionPackage": xpi,
            "openedInFirefox": opened
        }),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn firefox_install_action(_input: &str) -> Result<(), RuntimeError> {
    Err(RuntimeError::InvalidInput(
        "Firefox companion installation is unavailable on this platform".to_owned(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_firefox_extension(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/Applications/Firefox.app/Contents/MacOS/firefox");
    #[cfg(target_os = "linux")]
    let mut command = Command::new("firefox");
    command
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn open_firefox_extension(_path: &Path) -> bool {
    false
}

const REDDIT_REDIRECT_URI: &str = "http://127.0.0.1:43117/oauth/reddit";
const REDDIT_USER_AGENT: &str = concat!(
    "desktop:io.hydra.Hydra:",
    env!("CARGO_PKG_VERSION"),
    " (by /u/raisondecalcul)"
);

fn reddit_oauth_begin_action(input: &str) -> Result<(), RuntimeError> {
    let input: RedditOAuthBeginInput = serde_json::from_str(input)?;
    let client_id = reddit_client_id(input.client_id.as_deref())?;
    let request = RedditDataApi::authorization_request(&client_id, REDDIT_REDIRECT_URI)?;
    print_action_result(
        "reddit.oauth.begin",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({
            "authorizationUrl": request.authorization_url,
            "state": request.state,
            "redirectUri": REDDIT_REDIRECT_URI
        }),
    )
}

fn reddit_oauth_complete_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: RedditOAuthCompleteInput = serde_json::from_str(input)?;
    let client_id = reddit_client_id(input.client_id.as_deref())?;
    let tokens = RedditDataApi::exchange_code(
        &client_id,
        REDDIT_REDIRECT_URI,
        &input.expected_state,
        &input.callback_state,
        &input.code,
        REDDIT_USER_AGENT,
    )?;
    let api = RedditDataApi::new(
        client_id.clone(),
        REDDIT_REDIRECT_URI.to_owned(),
        REDDIT_USER_AGENT.to_owned(),
        tokens.access_token.clone(),
    )?;
    let identity = hydra_reddit::RedditAdapter::identity(&api)?;
    let credential = credential(identity, &client_id, tokens, unix_now());
    let mut store = DurableStore::open(root)?;
    let identity = RedditLinkService::new(PlatformRedditCredentialStore).link(
        &PersonaService::new(PlatformSecretStore),
        &mut store,
        PersonaId::parse(&input.persona_id)?,
        &api,
        &credential,
        unix_now(),
    )?;
    print_action_result(
        "reddit.oauth.complete",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": true, "username": identity.username}),
    )
}

fn reddit_oauth_connect_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: RedditOAuthConnectInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let client_id = reddit_client_id(input.client_id.as_deref())?;
    let request = RedditDataApi::authorization_request(&client_id, REDDIT_REDIRECT_URI)?;
    let listener = TcpListener::bind("127.0.0.1:43117")?;
    listener.set_nonblocking(true)?;
    launch_external(&request.authorization_url)?;
    let code = await_oauth_callback(&listener, &request.state, Duration::from_secs(300))?;
    let tokens = RedditDataApi::exchange_code(
        &client_id,
        REDDIT_REDIRECT_URI,
        &request.state,
        &request.state,
        &code,
        REDDIT_USER_AGENT,
    )?;
    let api = RedditDataApi::new(
        client_id.clone(),
        REDDIT_REDIRECT_URI.to_owned(),
        REDDIT_USER_AGENT.to_owned(),
        tokens.access_token.clone(),
    )?;
    let identity = hydra_reddit::RedditAdapter::identity(&api)?;
    let credential = credential(identity, &client_id, tokens, unix_now());
    let mut store = DurableStore::open(root)?;
    let identity = RedditLinkService::new(PlatformRedditCredentialStore).link(
        &PersonaService::new(PlatformSecretStore),
        &mut store,
        persona,
        &api,
        &credential,
        unix_now(),
    )?;
    print_action_result(
        "reddit.oauth.connect",
        operation_view(OperationId::new(), OperationState::Succeeded, true),
        serde_json::json!({"changed": true, "username": identity.username}),
    )
}

fn reddit_oauth_unlink_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: RedditOAuthUnlinkInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let mut store = DurableStore::open(root)?;
    RedditLinkService::new(PlatformRedditCredentialStore).unlink(
        &PersonaService::new(PlatformSecretStore),
        &mut store,
        persona,
        unix_now(),
    )?;
    print_changed("reddit.oauth.unlink")
}

fn reddit_identity_proof_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: RedditIdentityProofInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let mut artifact_url = Url::parse(&input.artifact_url)?;
    if artifact_url.scheme() != "https"
        || !matches!(
            artifact_url.host_str(),
            Some("www.reddit.com" | "old.reddit.com")
        )
        || !artifact_url.username().is_empty()
        || artifact_url.password().is_some()
        || artifact_url.port().is_some_and(|port| port != 443)
    {
        return Err(RuntimeError::InvalidInput(
            "identity proof must be a public HTTPS Reddit permalink".to_owned(),
        ));
    }
    let fullname = reddit_fullname_from_url(&artifact_url)?;
    let api = reddit_api(persona)?;
    let identity = hydra_reddit::RedditAdapter::identity(&api)?;
    let artifact = hydra_reddit::RedditAdapter::fetch(&api, &fullname)?;
    if artifact.author.as_deref() != Some(identity.username.as_str()) {
        return Err(RuntimeError::InvalidInput(
            "identity proof artifact was not authored by the linked Reddit account".to_owned(),
        ));
    }
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    let public_key = store
        .state()
        .personas
        .get(persona)
        .ok_or(hydra_domain::DomainError::MissingPersona)?
        .public_key
        .as_str()
        .to_owned();
    let challenge =
        format!("Verifying that I control the following Nostr public key: {public_key}");
    if !artifact.body.contains(&challenge) {
        return Err(RuntimeError::InvalidInput(
            "identity proof artifact does not contain this persona's exact challenge".to_owned(),
        ));
    }
    artifact_url
        .set_host(Some("www.reddit.com"))
        .map_err(|_| RuntimeError::InvalidInput("Reddit proof host is invalid".to_owned()))?;
    artifact_url.set_query(None);
    artifact_url.set_fragment(None);
    let proof = hydra_domain::RedditIdentityProof {
        persona,
        username: identity.username,
        artifact_url: artifact_url.to_string(),
        published_at: unix_now(),
    };
    PersonaService::new(PlatformSecretStore).publish_reddit_identity_proof(
        &mut store,
        proof,
        settings.write_relays_for(persona),
    )?;
    print_changed("reddit.identity_proof.publish")
}

fn await_oauth_callback(
    listener: &TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<String, RuntimeError> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        match listener.accept() {
            Ok((mut stream, _)) => match read_oauth_callback(&mut stream, expected_state) {
                Ok(code) => {
                    write_oauth_response(&mut stream, true)?;
                    return Ok(code);
                }
                Err(error) => {
                    write_oauth_response(&mut stream, false)?;
                    if matches!(
                        error,
                        RuntimeError::Reddit(RedditError::AuthorizationRejected)
                    ) {
                        return Err(error);
                    }
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(RuntimeError::InvalidInput(
        "Reddit authorization timed out without changing Hydra".to_owned(),
    ))
}

fn read_oauth_callback(
    stream: &mut TcpStream,
    expected_state: &str,
) -> Result<String, RuntimeError> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut bytes = [0_u8; 8192];
    let count = stream.read(&mut bytes)?;
    let request = std::str::from_utf8(&bytes[..count])
        .map_err(|_| RuntimeError::InvalidInput("OAuth callback is not UTF-8".to_owned()))?;
    parse_oauth_request(request, expected_state)
}

fn parse_oauth_request(request: &str, expected_state: &str) -> Result<String, RuntimeError> {
    let mut request_line = request
        .lines()
        .next()
        .map(str::split_whitespace)
        .ok_or_else(|| RuntimeError::InvalidInput("OAuth callback is malformed".to_owned()))?;
    let method = request_line.next();
    let target = request_line.next();
    let version = request_line.next();
    if method != Some("GET")
        || version.is_none_or(|value| !matches!(value, "HTTP/1.0" | "HTTP/1.1"))
        || request_line.next().is_some()
    {
        return Err(RuntimeError::InvalidInput(
            "OAuth callback request line is invalid".to_owned(),
        ));
    }
    let target = target
        .ok_or_else(|| RuntimeError::InvalidInput("OAuth callback is malformed".to_owned()))?;
    let callback = Url::parse(&format!("http://127.0.0.1{target}"))?;
    if callback.path() != "/oauth/reddit" {
        return Err(RuntimeError::InvalidInput(
            "OAuth callback path is invalid".to_owned(),
        ));
    }
    let values = callback.query_pairs().collect::<Vec<_>>();
    let values_for = |key: &str| {
        values
            .iter()
            .filter(|(name, _)| name.as_ref() == key)
            .map(|(_, value)| value.as_ref())
            .collect::<Vec<_>>()
    };
    if !values_for("error").is_empty() {
        return Err(RedditError::AuthorizationRejected.into());
    }
    if values_for("state") != [expected_state] {
        return Err(RuntimeError::InvalidInput(
            "OAuth callback state did not match".to_owned(),
        ));
    }
    let codes = values_for("code");
    match codes.as_slice() {
        [code] if !code.is_empty() && code.len() <= 4096 => Ok((*code).to_owned()),
        _ => Err(RedditError::AuthorizationRejected.into()),
    }
}

fn write_oauth_response(stream: &mut TcpStream, success: bool) -> Result<(), RuntimeError> {
    let (status, message) = if success {
        ("200 OK", "Reddit is connected. You may return to Hydra.")
    } else {
        (
            "400 Bad Request",
            "Hydra could not accept this authorization response.",
        )
    };
    let body = format!("<!doctype html><meta charset=utf-8><title>Hydra</title><p>{message}</p>");
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Security-Policy: default-src 'none'; frame-ancestors 'none'; base-uri 'none'\r\nX-Content-Type-Options: nosniff\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()?;
    Ok(())
}

fn reddit_browse_community_action(input: &str) -> Result<(), RuntimeError> {
    let input: RedditBrowseCommunityInput = serde_json::from_str(input)?;
    let api = reddit_api(PersonaId::parse(&input.persona_id)?)?;
    let page = hydra_reddit::RedditAdapter::community(
        &api,
        &input.subreddit,
        &input.sort,
        input.after.as_deref(),
    )?;
    let (rules, rules_available) =
        match hydra_reddit::RedditAdapter::community_rules(&api, &input.subreddit) {
            Ok(rules) => (rules, true),
            Err(_) => (Vec::new(), false),
        };
    print_action_result(
        "reddit.browse.community",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": false, "items": page.things, "after": page.after, "rules": rules, "rulesAvailable": rules_available}),
    )
}

fn reddit_browse_thread_action(input: &str) -> Result<(), RuntimeError> {
    let input: RedditBrowseThreadInput = serde_json::from_str(input)?;
    let items = hydra_reddit::RedditAdapter::thread(
        &reddit_api(PersonaId::parse(&input.persona_id)?)?,
        &RedditFullname::parse(input.post)?,
    )?;
    print_action_result(
        "reddit.browse.thread",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": false, "items": items}),
    )
}

fn reddit_export_preview_action(input: &str) -> Result<(), RuntimeError> {
    let input: RedditExportPreviewInput = serde_json::from_str(input)?;
    let preview = hydra_reddit::preview_export(&input.path)
        .map_err(|error| RuntimeError::InvalidInput(error.to_string()))?;
    print_action_result(
        "reddit.export.preview",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({
            "changed": false,
            "posts": preview.posts,
            "comments": preview.comments,
            "items": preview.items
        }),
    )
}

fn reddit_export_import_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: RedditExportImportInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let preview = hydra_reddit::preview_export(&input.path)
        .map_err(|error| RuntimeError::InvalidInput(error.to_string()))?;
    let selected = input.selected.into_iter().collect::<BTreeSet<_>>();
    if selected.is_empty() {
        return Err(RuntimeError::InvalidInput(
            "select at least one exported post or comment".to_owned(),
        ));
    }
    let settings_store = SettingsStore::new(root);
    let mut settings = settings_store.load()?;
    let relays = if input.publish {
        settings.write_relays_for(persona).to_vec()
    } else {
        Vec::new()
    };
    let imported = settings
        .reddit_export_imports
        .entry(persona.to_string())
        .or_default()
        .clone();
    let mut store = DurableStore::open(root)?;
    let service = DiscussionService::new(PlatformSecretStore);
    let mut succeeded = 0_usize;
    let mut skipped = 0_usize;
    let mut failed = Vec::new();
    for item in preview.items {
        if !selected.contains(&item.fullname) || imported.contains(&item.fullname) {
            skipped += 1;
            continue;
        }
        let fullname = item.fullname.clone();
        match import_export_item(&service, &mut store, persona, item, &relays) {
            Ok(()) => {
                succeeded += 1;
                settings
                    .reddit_export_imports
                    .entry(persona.to_string())
                    .or_default()
                    .insert(fullname);
                settings_store.save(&settings)?;
            }
            Err(error) => failed.push(serde_json::json!({
                "fullname": fullname,
                "reason": error.to_string()
            })),
        }
    }
    print_action_result(
        "reddit.export.import",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({
            "changed": succeeded > 0,
            "imported": succeeded,
            "skipped": skipped,
            "failed": failed,
            "published": input.publish
        }),
    )
}

fn import_export_item(
    service: &DiscussionService<PlatformSecretStore>,
    store: &mut DurableStore,
    persona: PersonaId,
    item: hydra_reddit::ExportItem,
    relays: &[String],
) -> Result<(), RuntimeError> {
    let subreddit = item
        .subreddit
        .as_deref()
        .ok_or_else(|| RuntimeError::InvalidInput("missing community".to_owned()))?;
    let community = CommunityKey::parse(subreddit)
        .map_err(|_| RuntimeError::InvalidInput("invalid community".to_owned()))?;
    let recorded_at = item.created_at.unwrap_or_else(unix_now);
    match item.kind {
        hydra_reddit::ExportItemKind::Post => service
            .import_authored_post(
                store,
                ImportAuthoredPost {
                    persona_id: persona,
                    title: item
                        .title
                        .unwrap_or_else(|| "Untitled Reddit post".to_owned()),
                    body: item.body,
                    communities: vec![community],
                    source: ExternalId::new("reddit", item.permalink)?,
                    relays: relays.to_vec(),
                    recorded_at,
                },
            )
            .map(|_| ())
            .map_err(RuntimeError::from),
        hydra_reddit::ExportItemKind::Comment => service
            .create_external_comment(
                store,
                CreateExternalComment {
                    persona_id: persona,
                    root: ExternalId::new(
                        "reddit",
                        item.root_permalink.ok_or_else(|| {
                            RuntimeError::InvalidInput("missing post permalink".to_owned())
                        })?,
                    )?,
                    parent: ExternalId::new(
                        "reddit",
                        item.parent_permalink.ok_or_else(|| {
                            RuntimeError::InvalidInput("missing parent permalink".to_owned())
                        })?,
                    )?,
                    source: Some(ExternalId::new("reddit", item.permalink)?),
                    communities: vec![community],
                    body: item.body,
                    relays: relays.to_vec(),
                    recorded_at,
                },
            )
            .map(|_| ())
            .map_err(RuntimeError::from),
    }
}

fn default_reddit_sort() -> String {
    "hot".to_owned()
}

fn default_search_limit() -> usize {
    50
}

fn default_raw_event_limit() -> usize {
    200
}

fn default_open_nostr_limit() -> usize {
    30
}

fn raw_events_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: RawEventsInput = serde_json::from_str(input)?;
    let store = DurableStore::open(root)?;
    let events = store.raw_events()?;
    let total = events.len();
    let events = events
        .into_iter()
        .rev()
        .take(input.limit.clamp(1, 1_000))
        .map(|envelope| {
            serde_json::json!({
                "id": envelope.id,
                "recordedAt": envelope.recorded_at,
                "checksum": envelope.checksum,
                "event": envelope.event,
            })
        })
        .collect::<Vec<_>>();
    print_action_result(
        "events.raw",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": false, "total": total, "events": events}),
    )
}

fn search_local_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: SearchInput = serde_json::from_str(input)?;
    let query = LocalSearchQuery::parse(&input.query)?;
    let store = DurableStore::open(root)?;
    let mut hits = store
        .state()
        .heads
        .current_heads()
        .filter(|head| search_matches_head(&store, head, &query))
        .map(|head| {
            serde_json::json!({
                "source": "hydra",
                "id": head.anchor.as_str(),
                "title": head.title,
                "body": head.body.as_str(),
                "communities": head.communities.iter().map(CommunityKey::as_str).collect::<Vec<_>>(),
                "editedAt": head.edited_at
            })
        })
        .collect::<Vec<_>>();
    if let Some(persona) = input.persona_id.as_deref() {
        let persona = PersonaId::parse(persona)?;
        hits.extend(
            private_state(&PlatformSecretStore, &store, persona)?
                .drafts
                .values()
                .filter(|draft| search_matches_draft(draft, &query))
                .map(|draft| {
                    serde_json::json!({
                        "source": "draft",
                        "id": draft.id,
                        "title": draft.title,
                        "body": draft.body,
                        "communities": draft.communities.iter().map(CommunityKey::as_str).collect::<Vec<_>>(),
                        "editedAt": draft.updated_at
                    })
                }),
        );
    }
    hits.sort_by_key(|hit| std::cmp::Reverse(hit["editedAt"].as_u64().unwrap_or_default()));
    hits.truncate(input.limit.clamp(1, 200));
    print_action_result(
        "search.local",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": false, "hits": hits}),
    )
}

fn search_matches_head(
    store: &DurableStore,
    head: &hydra_domain::ObjectHead,
    query: &LocalSearchQuery,
) -> bool {
    match query {
        LocalSearchQuery::Text(value) => {
            text_matches(head.title.as_deref(), head.body.as_str(), value)
        }
        LocalSearchQuery::Persona(value) => identity_matches(store, &head.author, value),
        LocalSearchQuery::Topic(value) => head
            .communities
            .iter()
            .any(|community| community.as_str().eq_ignore_ascii_case(value)),
        LocalSearchQuery::Reddit(value) => head
            .external_root
            .iter()
            .chain(head.external_parent.iter())
            .any(|external| reddit_identifier_matches(&external.canonical, value)),
        LocalSearchQuery::Provenance(value) => matches!(value.as_str(), "hydra" | "native"),
    }
}

fn search_matches_draft(draft: &DraftRecord, query: &LocalSearchQuery) -> bool {
    match query {
        LocalSearchQuery::Text(value) => text_matches(draft.title.as_deref(), &draft.body, value),
        LocalSearchQuery::Topic(value) => draft
            .communities
            .iter()
            .any(|community| community.as_str().eq_ignore_ascii_case(value)),
        LocalSearchQuery::Provenance(value) => value == "draft",
        LocalSearchQuery::Persona(_) | LocalSearchQuery::Reddit(_) => false,
    }
}

fn text_matches(title: Option<&str>, body: &str, query: &str) -> bool {
    title.is_some_and(|title| title.to_lowercase().contains(query))
        || body.to_lowercase().contains(query)
}

fn identity_matches(store: &DurableStore, key: &NostrPublicKey, query: &str) -> bool {
    key.as_str().to_lowercase().contains(query)
        || store
            .state()
            .personas
            .iter()
            .find(|persona| persona.public_key == *key)
            .is_some_and(|persona| persona.display_name.to_lowercase().contains(query))
}

fn reddit_identifier_matches(canonical: &str, query: &str) -> bool {
    let canonical = canonical.to_lowercase();
    if canonical.contains(query) || query.contains(&canonical) {
        return true;
    }
    reddit_identifier_fullname(&canonical)
        .zip(reddit_identifier_fullname(query))
        .is_some_and(|(canonical, query)| canonical == query)
}

fn reddit_identifier_fullname(value: &str) -> Option<String> {
    let value = value.trim().to_lowercase();
    if value.starts_with("t1_") || value.starts_with("t3_") {
        return Some(value);
    }
    Url::parse(&value)
        .ok()
        .and_then(|url| reddit_fullname_from_url(&url).ok())
        .map(|fullname| fullname.as_str().to_owned())
}

async fn search_network_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: SearchInput = serde_json::from_str(input)?;
    let settings = SettingsStore::new(root).load()?;
    let store = DurableStore::open(root)?;
    let relays = settings
        .active_persona_id
        .as_deref()
        .and_then(|value| PersonaId::parse(value).ok())
        .map_or_else(
            || Ok(settings.relays.clone()),
            |persona| allowed_read_relays(&settings, &store, persona),
        )?;
    if relays.is_empty() {
        return Err(RuntimeError::InvalidInput(
            "all read relays are blocked for this persona".to_owned(),
        ));
    }
    let events = hydra_nostr::search_events(&relays, &input.query, input.limit).await?;
    let hits = events
        .into_iter()
        .map(|event| {
            serde_json::json!({
                "source": "nostr",
                "id": event.id.to_hex(),
                "author": event.pubkey.to_hex(),
                "body": event.content,
                "createdAt": event.created_at.as_secs(),
                "event": event.as_json()
            })
        })
        .collect::<Vec<_>>();
    print_action_result(
        "search.network",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": false, "hits": hits}),
    )
}

async fn open_nostr_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: OpenNostrInput = serde_json::from_str(input)?;
    let limit = input.limit.clamp(1, 100);
    let settings = SettingsStore::new(root).load()?;
    let store = DurableStore::open(root)?;
    let persona = input
        .persona_id
        .as_deref()
        .map(PersonaId::parse)
        .transpose()?
        .or_else(|| active_persona_id(&store, &settings));
    let relays = persona.map_or_else(
        || Ok(settings.relays.clone()),
        |persona| allowed_read_relays(&settings, &store, persona),
    )?;
    if relays.is_empty() {
        return Err(RuntimeError::InvalidInput(
            "all read relays are blocked for this persona".to_owned(),
        ));
    }
    let events = hydra_nostr::fetch_open_events(&relays, input.since, limit).await?;
    let events = bounded_open_page(events, limit);
    let items = events
        .into_iter()
        .map(|(event, body)| open_nostr_item(&event, &body, &relays))
        .collect::<Vec<_>>();
    print_action_result(
        "nostr.open",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": false, "items": items}),
    )
}

async fn resolve_nostr_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: ResolveNostrInput = serde_json::from_str(input)?;
    let store = DurableStore::open(root)?;
    let settings = SettingsStore::new(root).load()?;
    let persona = input
        .persona_id
        .as_deref()
        .map(PersonaId::parse)
        .transpose()?
        .or_else(|| active_persona_id(&store, &settings));
    let relays = persona.map_or_else(
        || Ok(settings.relays.clone()),
        |persona| allowed_read_relays(&settings, &store, persona),
    )?;
    let event = hydra_nostr::fetch_portable_event(&input.uri, &relays).await?;
    let body = open_event_body(&event).ok_or_else(|| {
        RuntimeError::InvalidInput("portable event has no visible supported content".to_owned())
    })?;
    let item = open_nostr_item(&event, &body, &relays);
    print_action_result(
        "nostr.resolve",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": false, "item": item}),
    )
}

fn open_nostr_item(event: &Event, body: &str, relays: &[String]) -> serde_json::Value {
    let topics = event
        .tags
        .iter()
        .filter(|tag| tag.as_slice().first().map(String::as_str) == Some("t"))
        .filter_map(|tag| tag.content())
        .map(str::to_owned)
        .take(32)
        .collect::<Vec<_>>();
    let canon = hydra_nostr::received_canon_record(event).ok().flatten();
    let portable = hydra_nostr::portable_event_uri(event, relays).ok();
    let book_club_url = portable
        .as_deref()
        .and_then(|uri| uri.strip_prefix("nostr:"))
        .map(|entity| format!("bookclub://nostr/{entity}"));
    let canon_view = canon.map(|record| {
        serde_json::json!({
            "role": record.role,
            "objectId": record.object_id,
            "title": record.title,
            "creators": record.creators,
            "identifiers": record.identifiers,
            "summary": record.summary,
        })
    });
    serde_json::json!({
        "id": event.id.to_hex(),
        "kind": u16::from(event.kind),
        "author": event.pubkey.to_hex(),
        "body": body,
        "topics": topics,
        "uncategorized": topics.is_empty(),
        "createdAt": event.created_at.as_secs(),
        "event": event.as_json(),
        "portable": portable,
        "bookClubUrl": book_club_url,
        "canon": canon_view
    })
}

fn bounded_open_page(mut events: Vec<Event>, limit: usize) -> Vec<(Event, String)> {
    events.sort_by_key(|event| std::cmp::Reverse(event.created_at.as_secs()));
    let mut seen = BTreeSet::new();
    events.retain(|event| seen.insert(event.id.to_hex()));
    events
        .into_iter()
        .filter_map(|event| open_event_body(&event).map(|body| (event, body)))
        .take(limit)
        .collect()
}

fn open_event_body(event: &Event) -> Option<String> {
    if hydra_nostr::is_canon_record(event) {
        return hydra_nostr::received_canon_record(event)
            .ok()
            .flatten()
            .map(|record| record.title);
    }
    let fallback = event.tags.iter().find_map(|tag| {
        let parts = tag.as_slice();
        match parts.first().map(String::as_str) {
            Some("title" | "summary" | "alt" | "description") => parts
                .get(1)
                .filter(|value| !value.trim().is_empty())
                .cloned(),
            Some("imeta") => parts
                .iter()
                .skip(1)
                .find_map(|value| value.strip_prefix("alt "))
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    parts
                        .iter()
                        .skip(1)
                        .find_map(|value| value.strip_prefix("url "))
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| format!("Media: {value}"))
                }),
            _ => None,
        }
    });
    let source = if event.content.trim().is_empty() {
        fallback.as_deref()?
    } else {
        &event.content
    };
    Some(source.chars().take(4_000).collect())
}

fn keep_nostr_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: KeepNostrInput = serde_json::from_str(input)?;
    let mut store = DurableStore::open(root)?;
    ImportService::receive_public(&mut store, &input.event_json, unix_now())?;
    print_action_result(
        "nostr.keep",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": true}),
    )
}

fn curate_nostr_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: NostrCurateInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let communities = input
        .communities
        .into_iter()
        .map(CommunityKey::parse)
        .collect::<Result<Vec<_>, _>>()?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    ImportService::receive_public(&mut store, &input.event_json, unix_now())?;
    let outbound = DiscussionService::new(PlatformSecretStore).curate(
        &mut store,
        &CurateEvent {
            persona_id: persona,
            source_event_json: input.event_json,
            communities,
            relays: settings.write_relays_for(persona).to_vec(),
            recorded_at: unix_now(),
        },
    )?;
    print_action_result(
        "nostr.curate",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": true, "eventId": outbound.event_id}),
    )
}

fn categorize_nostr_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: NostrCategorizeInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let communities = input
        .communities
        .into_iter()
        .map(CommunityKey::parse)
        .collect::<Result<Vec<_>, _>>()?;
    if communities.is_empty() || communities.len() > hydra_domain::ObjectHead::MAX_COMMUNITIES {
        return Err(RuntimeError::InvalidInput(
            "choose between 1 and 32 topics".to_owned(),
        ));
    }
    let event = nostr::Event::from_json(&input.event_json)
        .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?;
    let mut store = DurableStore::open(root)?;
    ImportService::receive_public(&mut store, &input.event_json, unix_now())?;
    if !store
        .state()
        .heads
        .current_heads()
        .any(|head| head.anchor.as_str() == event.id.to_hex())
    {
        return Err(RuntimeError::InvalidInput(
            "this event type cannot be categorized in Hydra".to_owned(),
        ));
    }
    let settings_store = SettingsStore::new(root);
    let mut settings = settings_store.load()?;
    settings
        .local_topic_assignments
        .entry(persona.to_string())
        .or_default()
        .insert(
            event.id.to_hex(),
            communities
                .iter()
                .map(|community| community.as_str().to_owned())
                .collect(),
        );
    settings_store.save(&settings)?;
    print_action_result(
        "nostr.categorize_local",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": true, "eventId": event.id.to_hex()}),
    )
}

fn reddit_queue_post_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: RedditQueuePostInput = serde_json::from_str(input)?;
    let settings = SettingsStore::new(root).load()?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let anchor = AnchorId::parse(input.anchor)?;
    let mut store = DurableStore::open(root)?;
    let link = resolve_projection_link(
        &store,
        &anchor,
        input.attribution.as_deref(),
        input.link,
        settings.write_relays_for(persona),
    )?;
    let bridge = BridgeService::new(PlatformSecretStore, reddit_api(persona)?);
    let projection = bridge.queue_post(
        &mut store,
        QueuePost {
            persona,
            anchor: &anchor,
            subreddit: &input.subreddit,
            attribution: reddit_attribution(input.attribution.as_deref(), link.as_deref())?,
            relays: settings.write_relays_for(persona).to_vec(),
            recorded_at: unix_now(),
        },
    )?;
    print_projection_result("reddit.post.queue", &projection)
}

fn reddit_queue_comment_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: RedditQueueCommentInput = serde_json::from_str(input)?;
    let settings = SettingsStore::new(root).load()?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let anchor = AnchorId::parse(input.anchor)?;
    let parent = RedditFullname::parse(input.parent)?;
    let mut store = DurableStore::open(root)?;
    let link = resolve_projection_link(
        &store,
        &anchor,
        input.attribution.as_deref(),
        input.link,
        settings.write_relays_for(persona),
    )?;
    let bridge = BridgeService::new(PlatformSecretStore, reddit_api(persona)?);
    let projection = bridge.queue_comment(
        &mut store,
        QueueComment {
            persona,
            anchor: &anchor,
            parent: &parent,
            attribution: reddit_attribution(input.attribution.as_deref(), link.as_deref())?,
            relays: settings.write_relays_for(persona).to_vec(),
            recorded_at: unix_now(),
        },
    )?;
    print_projection_result("reddit.comment.queue", &projection)
}

fn reddit_execute_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: ProjectionInput = serde_json::from_str(input)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    let id = hydra_domain::ProjectionId::parse(&input.projection_id)?;
    let persona = projection_persona(&store, id)?;
    let projection = BridgeService::new(PlatformSecretStore, reddit_api(persona)?).execute(
        &mut store,
        id,
        settings.write_relays_for(persona).to_vec(),
        unix_now(),
    )?;
    print_projection_result("reddit.projection.execute", &projection)
}

fn reddit_projection_sync_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: ProjectionInput = serde_json::from_str(input)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    let id = hydra_domain::ProjectionId::parse(&input.projection_id)?;
    let persona = projection_persona(&store, id)?;
    let projection = BridgeService::new(PlatformSecretStore, reddit_api(persona)?).synchronize(
        &mut store,
        ProjectionAction {
            projection: id,
            relays: settings.write_relays_for(persona).to_vec(),
            recorded_at: unix_now(),
        },
    )?;
    print_projection_result("reddit.projection.sync", &projection)
}

fn reddit_projection_resolve_duplicates_action(
    root: &PathBuf,
    input: &str,
) -> Result<(), RuntimeError> {
    let input: ResolveDuplicatesInput = serde_json::from_str(input)?;
    let keep = ProjectionId::parse(&input.keep_projection_id)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    let persona = projection_persona(&store, keep)?;
    let resolved = BridgeService::new(PlatformSecretStore, reddit_api(persona)?)
        .resolve_duplicates(
            &mut store,
            &ResolveDuplicatesAction {
                keep,
                relays: settings.write_relays_for(persona).to_vec(),
                recorded_at: unix_now(),
            },
        )?;
    print_action_result(
        "reddit.projection.resolve_duplicates",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({
            "changed": true,
            "keptProjectionId": keep.to_string(),
            "resolvedCount": resolved.len().saturating_sub(1)
        }),
    )
}

fn reddit_projection_sync_setting_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: ProjectionSyncSettingInput = serde_json::from_str(input)?;
    let id = ProjectionId::parse(&input.projection_id)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    let mut projection = store
        .state()
        .projections
        .get(&id)
        .cloned()
        .ok_or_else(|| RuntimeError::InvalidInput("projection not found".to_owned()))?;
    projection.sync_enabled = input.enabled;
    let relays = settings.write_relays_for(projection.persona).to_vec();
    let projection = ProjectionService::new(PlatformSecretStore).record(
        &mut store,
        projection,
        relays,
        unix_now(),
    )?;
    print_projection_result("reddit.projection.sync_setting", &projection)
}

fn reddit_divergence_adopt_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: ProjectionInput = serde_json::from_str(input)?;
    let id = ProjectionId::parse(&input.projection_id)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    let projection = store
        .state()
        .projections
        .get(&id)
        .cloned()
        .ok_or_else(|| RuntimeError::InvalidInput("projection not found".to_owned()))?;
    let reddit_body = projection.divergence.clone().ok_or_else(|| {
        RuntimeError::InvalidInput("projection has no Reddit edit to adopt".to_owned())
    })?;
    let body = projection
        .rendered_suffix
        .as_deref()
        .and_then(|suffix| reddit_body.strip_suffix(suffix))
        .map_or(reddit_body.clone(), |canonical| {
            canonical.trim_end().to_owned()
        });
    let head = store
        .state()
        .heads
        .current_head(&projection.anchor)?
        .clone();
    DiscussionService::new(PlatformSecretStore).edit_object(
        &mut store,
        EditObject {
            persona_id: projection.persona,
            anchor: projection.anchor.clone(),
            title: head.title,
            body,
            communities: None,
            relays: settings.write_relays_for(projection.persona).to_vec(),
            recorded_at: unix_now(),
        },
    )?;
    let projection = BridgeService::new(PlatformSecretStore, reddit_api(projection.persona)?)
        .push_current(
            &mut store,
            ProjectionAction {
                projection: id,
                relays: settings.write_relays_for(projection.persona).to_vec(),
                recorded_at: unix_now(),
            },
        )?;
    print_projection_result("reddit.divergence.adopt", &projection)
}

fn reddit_divergence_restore_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: ProjectionInput = serde_json::from_str(input)?;
    let id = ProjectionId::parse(&input.projection_id)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    let persona = projection_persona(&store, id)?;
    let projection = BridgeService::new(PlatformSecretStore, reddit_api(persona)?).push_current(
        &mut store,
        ProjectionAction {
            projection: id,
            relays: settings.write_relays_for(persona).to_vec(),
            recorded_at: unix_now(),
        },
    )?;
    print_projection_result("reddit.divergence.restore", &projection)
}

fn reddit_divergence_keep_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: ProjectionInput = serde_json::from_str(input)?;
    let id = ProjectionId::parse(&input.projection_id)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    let mut projection = store
        .state()
        .projections
        .get(&id)
        .cloned()
        .ok_or_else(|| RuntimeError::InvalidInput("projection not found".to_owned()))?;
    if projection.state != hydra_domain::ProjectionState::Diverged {
        return Err(RuntimeError::InvalidInput(
            "projection has no divergence to preserve".to_owned(),
        ));
    }
    let now = unix_now();
    projection.transition(hydra_domain::ProjectionState::Synchronizing)?;
    ProjectionService::new(PlatformSecretStore).record(
        &mut store,
        projection.clone(),
        settings.write_relays_for(projection.persona).to_vec(),
        now,
    )?;
    projection.divergence = None;
    projection.transition(hydra_domain::ProjectionState::Live)?;
    let relays = settings.write_relays_for(projection.persona).to_vec();
    let projection = ProjectionService::new(PlatformSecretStore).record(
        &mut store,
        projection,
        relays,
        now.saturating_add(1),
    )?;
    print_projection_result("reddit.divergence.keep", &projection)
}

fn reddit_big_stick_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: BigStickInput = serde_json::from_str(input)?;
    let settings = SettingsStore::new(root).load()?;
    if !settings.continuity.big_stick_enabled {
        return Err(RuntimeError::InvalidInput(
            "Big Stick is disabled in local settings".to_owned(),
        ));
    }
    let mut store = DurableStore::open(root)?;
    let id = hydra_domain::ProjectionId::parse(&input.projection_id)?;
    let persona = projection_persona(&store, id)?;
    let anchor = store
        .state()
        .projections
        .get(&id)
        .map(|projection| projection.anchor.clone())
        .ok_or_else(|| RuntimeError::InvalidInput("projection not found".to_owned()))?;
    let portable_link = match input.portable_link {
        Some(value) => value,
        None => hydra_reddit::resolve_portable_link(
            &portable_link(&store, &anchor, settings.write_relays_for(persona))?,
            settings.continuity.preferred_gateway_template.as_deref(),
        )?,
    };
    let projection = BridgeService::new(PlatformSecretStore, reddit_api(persona)?)
        .attach_big_stick(
            &mut store,
            BigStickAction {
                projection: id,
                portable_link: &portable_link,
                replication_threshold: settings
                    .continuity
                    .replication_threshold
                    .unwrap_or(settings.replication_threshold),
                archive_level: preservation_level(
                    input
                        .archive_level
                        .as_deref()
                        .unwrap_or(&settings.continuity.big_stick_archive_level),
                )?,
                relays: settings.write_relays_for(persona).to_vec(),
                recorded_at: unix_now(),
            },
        )?;
    print_projection_result("reddit.big_stick", &projection)
}

fn reddit_withdraw_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: WithdrawInput = serde_json::from_str(input)?;
    let settings = SettingsStore::new(root).load()?;
    if !settings.continuity.reddacted_enabled {
        return Err(RuntimeError::InvalidInput(
            "Reddacted is disabled in local settings".to_owned(),
        ));
    }
    let mut store = DurableStore::open(root)?;
    let id = hydra_domain::ProjectionId::parse(&input.projection_id)?;
    let persona = projection_persona(&store, id)?;
    let anchor = store
        .state()
        .projections
        .get(&id)
        .map(|projection| projection.anchor.clone())
        .ok_or_else(|| RuntimeError::InvalidInput("projection not found".to_owned()))?;
    let portable_link = match input.portable_link {
        Some(value) => value,
        None => hydra_reddit::resolve_portable_link(
            &portable_link(&store, &anchor, settings.write_relays_for(persona))?,
            settings.continuity.preferred_gateway_template.as_deref(),
        )?,
    };
    let marker = withdrawal_marker(&input.marker, &portable_link)?;
    let projection = BridgeService::new(PlatformSecretStore, reddit_api(persona)?).withdraw(
        &mut store,
        WithdrawalAction {
            projection: id,
            marker,
            replication_threshold: settings
                .continuity
                .replication_threshold
                .unwrap_or(settings.replication_threshold),
            archive_level: preservation_level(
                input
                    .archive_level
                    .as_deref()
                    .unwrap_or(&settings.continuity.reddacted_archive_level),
            )?,
            relays: settings.write_relays_for(persona).to_vec(),
            recorded_at: unix_now(),
        },
    )?;
    print_projection_result("reddit.withdraw", &projection)
}

fn backup_export_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: BackupInput = serde_json::from_str(input)?;
    BackupService::export(
        root,
        &PlatformSecretStore,
        PersonaId::parse(input.persona_id.as_deref().ok_or_else(|| {
            RuntimeError::InvalidInput("choose the persona to back up".to_owned())
        })?)?,
        &input.passphrase,
        input.path,
    )?;
    let settings_store = SettingsStore::new(root);
    let mut settings = settings_store.load()?;
    settings.last_backup_at = Some(unix_now());
    settings_store.save(&settings)?;
    print_changed("backup.export")
}

fn backup_restore_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: BackupInput = serde_json::from_str(input)?;
    let current = DurableStore::open(root)?;
    if current.state().personas.iter().next().is_some() {
        return Err(RuntimeError::InvalidInput(
            "restore is available only before creating or importing a persona".to_owned(),
        ));
    }
    drop(current);

    // A first snapshot initializes an empty local root. Restore into a verified
    // sibling first, then atomically replace that disposable initialization.
    let parent = root
        .parent()
        .ok_or_else(|| RuntimeError::InvalidInput("Hydra state root has no parent".to_owned()))?;
    fs::create_dir_all(parent)?;
    let transaction = tempfile::Builder::new()
        .prefix(".hydra-runtime-restore-")
        .tempdir_in(parent)?;
    let restored = transaction.path().join("restored");
    BackupService::restore(
        input.path,
        &restored,
        &PlatformSecretStore,
        input.passphrase,
    )?;
    let previous = transaction.path().join("previous");
    if root.exists() {
        fs::rename(root, &previous)?;
    }
    if let Err(error) = fs::rename(&restored, root) {
        if previous.exists() {
            let _ = fs::rename(&previous, root);
        }
        return Err(error.into());
    }
    print_changed("backup.restore")
}

fn settings_update_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let mut input: SettingsUpdateInput = serde_json::from_str(input)?;
    let store = SettingsStore::new(root);
    let mut settings = store.load()?;
    apply_lens_setting_updates(&mut settings, &mut input);
    if let Some(value) = input.relays {
        settings.relays = value;
        settings.relay_probe = ReadinessProbe::default();
    }
    if input.persona_read_relays.is_some() || input.persona_write_relays.is_some() {
        let persona = PersonaId::parse(input.persona_id.as_deref().ok_or_else(|| {
            RuntimeError::InvalidInput("persona_id is required for persona relays".to_owned())
        })?)?;
        let read = input
            .persona_read_relays
            .unwrap_or_else(|| settings.relays.clone());
        let write = input
            .persona_write_relays
            .unwrap_or_else(|| settings.relays.clone());
        let mut durable = DurableStore::open(root)?;
        PersonaService::new(PlatformSecretStore).configure_relays(
            &mut durable,
            persona,
            read.clone(),
            write.clone(),
            unix_now(),
        )?;
        settings.persona_relays.insert(
            persona.to_string(),
            hydra_store::PersonaRelaySettings { read, write },
        );
        settings.relay_probe = ReadinessProbe::default();
    }
    if let Some(value) = input.inbox_relays {
        settings.inbox_relays = value;
    }
    if let Some(value) = input.replication_threshold {
        settings.replication_threshold = value;
        settings.relay_probe = ReadinessProbe::default();
    }
    if let Some(value) = input.theme {
        settings.theme = value;
    }
    if let Some(value) = input.onboarding_complete {
        settings.onboarding_complete = value;
    }
    if let Some(value) = input.crosspost_default {
        settings.crosspost_default = value;
    }
    if let Some(value) = input.book_club_cross_links_enabled {
        settings.cross_links.book_club_enabled = value;
    }
    if let Some(value) = input.persona_crosspost_defaults {
        settings.persona_crosspost_defaults = value;
    }
    if let Some(value) = input.community_crosspost_defaults {
        settings.community_crosspost_defaults = value;
    }
    if let Some(value) = input.content_crosspost_defaults {
        settings.content_crosspost_defaults = value;
    }
    if let Some(value) = input.media_copy_enabled {
        settings.media_copy_enabled = value;
    }
    if let Some(value) = input.max_media_bytes {
        settings.max_media_bytes = value;
    }
    if let Some(value) = input.persona_blob_servers {
        settings.persona_blob_servers = value;
    }
    if let Some(value) = input.big_stick_enabled {
        settings.continuity.big_stick_enabled = value;
    }
    if let Some(value) = input.reddacted_enabled {
        settings.continuity.reddacted_enabled = value;
    }
    if let Some(value) = input.big_stick_archive_level {
        settings.continuity.big_stick_archive_level = value;
    }
    if let Some(value) = input.reddacted_archive_level {
        settings.continuity.reddacted_archive_level = value;
    }
    if let Some(value) = input.continuity_replication_threshold {
        settings.continuity.replication_threshold = (value > 0).then_some(value);
    }
    if let Some(value) = input.preferred_gateway_template {
        settings.continuity.preferred_gateway_template =
            (!value.trim().is_empty()).then(|| value.trim().to_owned());
    }
    store.save(&settings)?;
    print_changed("settings.update")
}

fn apply_lens_setting_updates(settings: &mut Settings, input: &mut SettingsUpdateInput) {
    if let Some(value) = input.feed_source_weights.take() {
        settings.feed_source_weights = value;
    }
    if let Some(value) = input.spam_filter_threshold {
        settings.spam_filter_threshold = value;
    }
    if let Some(value) = input.remote_media_policy.take() {
        settings.remote_media_policy = value;
    }
}

async fn readiness_probe_action(root: &PathBuf) -> Result<(), RuntimeError> {
    let settings_store = SettingsStore::new(root);
    let mut settings = settings_store.load()?;
    let tested_at = unix_now();
    settings.relay_probe.last_tested_at = Some(tested_at);
    let relays = settings
        .active_persona_id
        .as_deref()
        .and_then(|value| PersonaId::parse(value).ok())
        .map_or(settings.relays.as_slice(), |persona| {
            settings.write_relays_for(persona)
        });
    match hydra_nostr::probe_relays(relays, Duration::from_secs(5)).await {
        Ok(probe) => {
            settings.relay_probe.ready = probe.connected >= settings.replication_threshold;
            settings.relay_probe.detail = format!(
                "{} of {} relays connected; {} required",
                probe.connected, probe.configured, settings.replication_threshold
            );
            if settings.relay_probe.ready {
                settings.relay_probe.last_success_at = Some(tested_at);
            }
        }
        Err(error) => {
            settings.relay_probe.ready = false;
            settings.relay_probe.detail = error.to_string();
        }
    }

    let store = DurableStore::open(root)?;
    let linked = store
        .state()
        .personas
        .iter()
        .filter(|persona| persona.reddit_account.is_some())
        .map(|persona| persona.id)
        .collect::<Vec<_>>();
    settings.reddit_probe.last_tested_at = Some(tested_at);
    if linked.is_empty() {
        settings.reddit_probe.ready = false;
        "Optional; no Reddit account linked".clone_into(&mut settings.reddit_probe.detail);
    } else {
        let mut connected = 0_usize;
        let mut errors = Vec::new();
        for persona in &linked {
            match reddit_api(*persona)
                .and_then(|api| hydra_reddit::RedditAdapter::identity(&api).map_err(Into::into))
            {
                Ok(_) => connected += 1,
                Err(error) => errors.push(error.to_string()),
            }
        }
        settings.reddit_probe.ready = connected == linked.len();
        settings.reddit_probe.detail = if errors.is_empty() {
            format!("{connected} linked Reddit account(s) connected")
        } else {
            format!(
                "{connected} of {} connected: {}",
                linked.len(),
                errors.join("; ")
            )
        };
        if settings.reddit_probe.ready {
            settings.reddit_probe.last_success_at = Some(tested_at);
        }
    }
    settings_store.save(&settings)?;
    print_action_result(
        "readiness.probe",
        operation_view(OperationId::new(), OperationState::Succeeded, true),
        serde_json::json!({"changed": true, "snapshot": state_envelope(root)?}),
    )
}

fn preserve_media_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: PreserveMediaInput = serde_json::from_str(input)?;
    let settings = SettingsStore::new(root).load()?;
    if !settings.media_copy_enabled {
        return Err(RuntimeError::InvalidInput(
            "media copying is disabled in settings".to_owned(),
        ));
    }
    let mut store = DurableStore::open(root)?;
    let media = MediaStore::new(root);
    let object = AnchorId::parse(input.object)?;
    let head = store.state().heads.current_head(&object)?.clone();
    let persona = store
        .state()
        .personas
        .iter()
        .find(|persona| persona.public_key == head.author)
        .ok_or(hydra_domain::DomainError::MissingPersona)?;
    let persona_id = persona.id;
    let blob_servers = settings
        .persona_blob_servers
        .get(&persona_id.to_string())
        .cloned()
        .unwrap_or_default();
    let preserved_at = unix_now();
    let manifest = ArchiveService::new(PlatformSecretStore).preserve_and_publish_media(
        &mut store,
        &media,
        PreserveAndPublishMedia {
            persona_id,
            source: PathBuf::from(input.source_path),
            object,
            mime_type: input.mime_type,
            original_url: input.original_url,
            blob_servers,
            relays: settings.write_relays_for(persona_id).to_vec(),
            max_bytes: settings.max_media_bytes,
            description: head.title.unwrap_or_else(|| "Hydra media".to_owned()),
            preserved_at,
        },
    )?;
    print_action_result(
        "media.preserve",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({
            "changed": true,
            "sha256": manifest.sha256,
            "local": manifest.local_path,
            "blobUrls": manifest.blob_urls,
            "nip94EventId": manifest.metadata_event_id
        }),
    )
}

fn community_subscription_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: CommunitySubscriptionInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    SocialService::new(PlatformSecretStore).set_community_subscription(
        &mut store,
        &SetCommunitySubscription {
            persona_id: persona,
            community: CommunityKey::parse(input.community)?,
            public: input.public,
            subscribed: input.subscribed,
            relays: settings.write_relays_for(persona).to_vec(),
            changed_at: unix_now(),
        },
    )?;
    print_changed("community.subscribe")
}

async fn send_message_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: SendMessageInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let settings = SettingsStore::new(root).load()?;
    let recipient = NostrPublicKey::parse(input.recipient)?;
    let mut store = DurableStore::open(root)?;
    let recipient_relays = if input.recipient_relays.is_empty() {
        let relays = allowed_read_relays(&settings, &store, persona)?;
        hydra_nostr::discover_inbox_relays(&relays, &recipient).await?
    } else {
        input.recipient_relays
    };
    MessagingService::new(PlatformSecretStore)
        .send(
            &mut store,
            SendDirectMessage {
                persona_id: persona,
                recipient,
                body: input.body,
                recipient_relays,
                sender_relays: settings.inbox_relays,
                created_at: unix_now(),
            },
        )
        .await?;
    print_changed("message.send")
}

fn refresh_action(root: &PathBuf) -> Result<(), RuntimeError> {
    print_action_result(
        "refresh_state",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"snapshot": state_envelope(root)?}),
    )
}

fn create_persona_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: CreatePersonaInput = serde_json::from_str(input)?;
    let settings_store = SettingsStore::new(root);
    let mut settings = settings_store.load()?;
    let mut store = DurableStore::open(root)?;
    let now = unix_now();
    let persona =
        PersonaService::new(PlatformSecretStore).create(&mut store, input.display_name, now)?;
    PersonaService::new(PlatformSecretStore).configure_relays(
        &mut store,
        persona.id,
        settings.relays.clone(),
        settings.relays.clone(),
        now,
    )?;
    PersonaService::new(PlatformSecretStore).publish_profile(
        &mut store,
        persona.id,
        persona.display_name.clone(),
        settings.write_relays_for(persona.id),
        now,
    )?;
    MessagingService::new(PlatformSecretStore).configure_inbox(
        &mut store,
        persona.id,
        settings.inbox_relays.clone(),
        &settings.relays,
        now,
    )?;
    if settings.active_persona_id.is_none() {
        settings.active_persona_id = Some(persona.id.to_string());
        settings_store.save(&settings)?;
    }
    print_changed("persona.create")
}

fn import_persona_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: ImportPersonaInput = serde_json::from_str(input)?;
    let settings_store = SettingsStore::new(root);
    let mut settings = settings_store.load()?;
    let mut store = DurableStore::open(root)?;
    let now = unix_now();
    let persona = PersonaService::new(PlatformSecretStore).import(
        &mut store,
        input.display_name,
        &input.secret,
        now,
    )?;
    PersonaService::new(PlatformSecretStore).configure_relays(
        &mut store,
        persona.id,
        settings.relays.clone(),
        settings.relays.clone(),
        now,
    )?;
    PersonaService::new(PlatformSecretStore).publish_profile(
        &mut store,
        persona.id,
        persona.display_name.clone(),
        settings.write_relays_for(persona.id),
        now,
    )?;
    MessagingService::new(PlatformSecretStore).configure_inbox(
        &mut store,
        persona.id,
        settings.inbox_relays.clone(),
        &settings.relays,
        now,
    )?;
    if settings.active_persona_id.is_none() {
        settings.active_persona_id = Some(persona.id.to_string());
        settings_store.save(&settings)?;
    }
    print_changed("persona.import")
}

async fn connect_remote_persona_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: RemotePersonaInput = serde_json::from_str(input)?;
    let settings_store = SettingsStore::new(root);
    let mut settings = settings_store.load()?;
    let mut store = DurableStore::open(root)?;
    let now = unix_now();
    let persona = PersonaService::new(PlatformSecretStore)
        .connect_remote(&mut store, input.display_name, &input.bunker_uri, now)
        .await?;
    PersonaService::new(PlatformSecretStore).configure_relays(
        &mut store,
        persona.id,
        settings.relays.clone(),
        settings.relays.clone(),
        now,
    )?;
    PersonaService::new(PlatformSecretStore).publish_profile(
        &mut store,
        persona.id,
        persona.display_name.clone(),
        settings.write_relays_for(persona.id),
        now,
    )?;
    MessagingService::new(PlatformSecretStore).configure_inbox(
        &mut store,
        persona.id,
        settings.inbox_relays.clone(),
        &settings.relays,
        now,
    )?;
    if settings.active_persona_id.is_none() {
        settings.active_persona_id = Some(persona.id.to_string());
        settings_store.save(&settings)?;
    }
    print_changed("persona.connect_remote")
}

fn update_persona_profile_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: UpdatePersonaProfileInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    PersonaService::new(PlatformSecretStore).publish_profile(
        &mut store,
        persona,
        input.display_name,
        settings.write_relays_for(persona),
        unix_now(),
    )?;
    print_changed("persona.profile.update")
}

fn switch_persona_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: SwitchPersonaInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let mut store = DurableStore::open(root)?;
    if !store.state().personas.contains(persona) {
        return Err(hydra_domain::DomainError::MissingPersona.into());
    }
    let settings_store = SettingsStore::new(root);
    let mut settings = settings_store.load()?;
    let operation = OperationId::new();
    let mut workflow = ContinuityWorkflow {
        id: operation,
        persona,
        subject: Some(persona.to_string()),
        state: ContinuityState::PersonaSwitch(PersonaSwitchState::Active),
    };
    store.append(
        DurableEvent::ContinuityWorkflowChanged(workflow.clone()),
        unix_now(),
    )?;
    workflow.transition(ContinuityState::PersonaSwitch(
        PersonaSwitchState::Switching,
    ))?;
    store.append(
        DurableEvent::ContinuityWorkflowChanged(workflow.clone()),
        unix_now(),
    )?;
    settings.active_persona_id = Some(persona.to_string());
    settings_store.save(&settings)?;
    workflow.transition(ContinuityState::PersonaSwitch(PersonaSwitchState::Active))?;
    store.append(
        DurableEvent::ContinuityWorkflowChanged(workflow),
        unix_now(),
    )?;
    print_changed("persona.switch")
}

fn save_draft_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: SaveDraftInput = serde_json::from_str(input)?;
    let now = unix_now();
    let draft = DraftRecord {
        id: input.id.unwrap_or_else(|| OperationId::new().to_string()),
        persona: PersonaId::parse(&input.persona_id)?,
        kind: parse_draft_kind(&input.kind)?,
        title: input.title,
        body: input.body,
        communities: input
            .communities
            .iter()
            .map(CommunityKey::parse)
            .collect::<Result<Vec<_>, _>>()?,
        parent: input.parent.map(AnchorId::parse).transpose()?,
        updated_at: now,
        discarded: false,
    };
    let id = draft.id.clone();
    let mut store = DurableStore::open(root)?;
    DraftService::save(&PlatformSecretStore, &mut store, draft, now)?;
    print_action_result(
        "draft.save",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": true, "draft_id": id}),
    )
}

fn discard_draft_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: DiscardDraftInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let store = DurableStore::open(root)?;
    let mut draft = private_state(&PlatformSecretStore, &store, persona)?
        .drafts
        .get(&input.id)
        .cloned()
        .ok_or_else(|| {
            RuntimeError::InvalidInput("draft does not exist for this persona".to_owned())
        })?;
    drop(store);
    draft.discarded = true;
    draft.updated_at = unix_now();
    let mut store = DurableStore::open(root)?;
    DraftService::save(&PlatformSecretStore, &mut store, draft, unix_now())?;
    print_changed("draft.discard")
}

fn parse_draft_kind(value: &str) -> Result<DraftKind, RuntimeError> {
    match value {
        "post" => Ok(DraftKind::Post),
        "comment" => Ok(DraftKind::Comment),
        "norm" => Ok(DraftKind::Norm),
        _ => Err(RuntimeError::InvalidInput(
            "draft kind must be post, comment, or norm".to_owned(),
        )),
    }
}

fn create_post_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: CreatePostInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let communities = input
        .communities
        .iter()
        .map(CommunityKey::parse)
        .collect::<Result<Vec<_>, _>>()?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    let head = DiscussionService::new(PlatformSecretStore).create_post(
        &mut store,
        CreatePost {
            persona_id: persona,
            title: input.title,
            body: input.body,
            communities,
            relays: settings.write_relays_for(persona).to_vec(),
            recorded_at: unix_now(),
        },
    )?;
    print_action_result(
        "post.create",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": true, "anchor": head.anchor.as_str()}),
    )
}

fn create_comment_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: CreateCommentInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    DiscussionService::new(PlatformSecretStore).create_comment(
        &mut store,
        CreateComment {
            persona_id: persona,
            parent_anchor: AnchorId::parse(input.parent_anchor)?,
            body: input.body,
            relays: settings.write_relays_for(persona).to_vec(),
            recorded_at: unix_now(),
        },
    )?;
    print_changed("comment.create")
}

fn create_external_comment_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: CreateExternalCommentInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let settings = SettingsStore::new(root).load()?;
    let communities = input
        .communities
        .into_iter()
        .map(CommunityKey::parse)
        .collect::<Result<Vec<_>, _>>()?;
    let mut store = DurableStore::open(root)?;
    let (root_system, root_id, parent_system, parent_id) = match (input.root_id, input.parent_id) {
        (Some(root_id), Some(parent_id)) => (
            input.root_system.ok_or_else(|| {
                RuntimeError::InvalidInput("external root system is required".to_owned())
            })?,
            root_id,
            input.parent_system.ok_or_else(|| {
                RuntimeError::InvalidInput("external parent system is required".to_owned())
            })?,
            parent_id,
        ),
        (None, None) => (
            "reddit".to_owned(),
            input.root_url.ok_or_else(|| {
                RuntimeError::InvalidInput("external root is required".to_owned())
            })?,
            "reddit".to_owned(),
            input.parent_url.ok_or_else(|| {
                RuntimeError::InvalidInput("external parent is required".to_owned())
            })?,
        ),
        _ => {
            return Err(RuntimeError::InvalidInput(
                "external root and parent must use the same input shape".to_owned(),
            ));
        }
    };
    let external_root = ExternalId::new(root_system, root_id)?;
    let external_parent = ExternalId::new(parent_system, parent_id)?;
    let head = DiscussionService::new(PlatformSecretStore).create_external_comment(
        &mut store,
        CreateExternalComment {
            persona_id: persona,
            root: external_root,
            parent: external_parent,
            source: None,
            communities,
            body: input.body,
            relays: settings.write_relays_for(persona).to_vec(),
            recorded_at: unix_now(),
        },
    )?;
    print_action_result(
        "comment.create_external",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({"changed": true, "anchor": head.anchor.as_str()}),
    )
}

fn create_norm_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: CreateNormInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    DiscussionService::new(PlatformSecretStore).create_norm(
        &mut store,
        CreateNorm {
            persona_id: persona,
            statement: input.statement,
            community: CommunityKey::parse(&input.community)?,
            relays: settings.write_relays_for(persona).to_vec(),
            recorded_at: unix_now(),
        },
    )?;
    print_changed("norm.create")
}

fn edit_object_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: EditObjectInput = serde_json::from_str(input)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let anchor = AnchorId::parse(input.anchor)?;
    DiscussionService::new(PlatformSecretStore).edit_object(
        &mut store,
        EditObject {
            persona_id: persona,
            anchor: anchor.clone(),
            title: input.title,
            body: input.body,
            communities: input
                .communities
                .map(|values| {
                    values
                        .into_iter()
                        .map(CommunityKey::parse)
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?,
            relays: settings.write_relays_for(persona).to_vec(),
            recorded_at: unix_now(),
        },
    )?;
    let active = store
        .state()
        .projections
        .values()
        .filter(|projection| {
            projection.anchor == anchor
                && projection.persona == persona
                && projection.sync_enabled
                && matches!(
                    projection.state,
                    hydra_domain::ProjectionState::Live
                        | hydra_domain::ProjectionState::Diverged
                        | hydra_domain::ProjectionState::Locked
                        | hydra_domain::ProjectionState::Removed
                )
        })
        .map(|projection| projection.id)
        .collect::<Vec<_>>();
    let mut updated = 0_u64;
    let mut failed = 0_u64;
    if !active.is_empty() {
        match reddit_api(persona) {
            Ok(adapter) => {
                let bridge = BridgeService::new(PlatformSecretStore, adapter);
                for projection in active {
                    match bridge.push_current(
                        &mut store,
                        ProjectionAction {
                            projection,
                            relays: settings.write_relays_for(persona).to_vec(),
                            recorded_at: unix_now(),
                        },
                    ) {
                        Ok(_) => updated += 1,
                        Err(_) => failed += 1,
                    }
                }
            }
            Err(_) => failed = u64::try_from(active.len()).unwrap_or(u64::MAX),
        }
    }
    print_action_result(
        "object.edit",
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        serde_json::json!({
            "changed": true,
            "redditUpdated": updated,
            "redditFailed": failed
        }),
    )
}

fn reaction_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: ReactionInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    DiscussionService::new(PlatformSecretStore).react(
        &mut store,
        ReactToObject {
            persona_id: persona,
            target: AnchorId::parse(input.target)?,
            value: match input.value.as_str() {
                "+" => ReactionValue::Upvote,
                "-" => ReactionValue::Downvote,
                "0" => ReactionValue::Neutral,
                _ => ReactionValue::Emoji(input.value),
            },
            relays: settings.write_relays_for(persona).to_vec(),
            recorded_at: unix_now(),
        },
    )?;
    print_changed("reaction.set")
}

fn disown_object_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: DisownObjectInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    DiscussionService::new(PlatformSecretStore).request_disowning(
        &mut store,
        RequestObjectDisowning {
            persona_id: persona,
            anchor: AnchorId::parse(input.anchor)?,
            reason: input.reason.unwrap_or_default(),
            relays: settings.write_relays_for(persona).to_vec(),
            requested_at: unix_now(),
        },
    )?;
    print_changed("object.disown")
}

fn revisit_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: RevisitInput = serde_json::from_str(input)?;
    let intent = match input.intent.as_str() {
        "return_soon" => RevisitIntent::ReturnSoon,
        "reconsider_vote" => RevisitIntent::ReconsiderVote,
        "review_on_date" => RevisitIntent::ReviewOnDate,
        "study" => RevisitIntent::Study,
        "notify_on_activity" => RevisitIntent::NotifyOnActivity,
        "collection" => RevisitIntent::Collection(input.collection.unwrap_or_default()),
        other => {
            return Err(RuntimeError::InvalidInput(format!(
                "unknown revisit intent {other}"
            )));
        }
    };
    let mut store = DurableStore::open(root)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    DiscussionService::new(PlatformSecretStore).set_revisit(
        &mut store,
        SetRevisit {
            persona_id: persona,
            target: AnchorId::parse(input.target)?,
            intent,
            due_at: input.due_at,
            recorded_at: unix_now(),
        },
    )?;
    print_changed("revisit.set")
}

fn remove_revisit_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: RemoveRevisitInput = serde_json::from_str(input)?;
    let mut store = DurableStore::open(root)?;
    DiscussionService::new(PlatformSecretStore).remove_revisit(
        &mut store,
        RemoveRevisit {
            persona_id: PersonaId::parse(&input.persona_id)?,
            target: AnchorId::parse(input.target)?,
            recorded_at: unix_now(),
        },
    )?;
    print_changed("revisit.remove")
}

fn follow_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: FollowInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    SocialService::new(PlatformSecretStore).set_follow(
        &mut store,
        &SetFollow {
            persona_id: persona,
            target: NostrPublicKey::parse(input.target)?,
            public: input.public,
            following: input.following,
            relays: settings.write_relays_for(persona).to_vec(),
            changed_at: unix_now(),
        },
    )?;
    print_changed("follow.set")
}

fn publish_follow_set_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: PublishFollowSetInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let settings = SettingsStore::new(root).load()?;
    let members = input
        .members
        .into_iter()
        .map(NostrPublicKey::parse)
        .collect::<Result<Vec<_>, _>>()?;
    let mut store = DurableStore::open(root)?;
    SocialService::new(PlatformSecretStore).publish_follow_set(
        &mut store,
        &PublishFollowSet {
            persona_id: persona,
            identifier: input.identifier,
            title: input.title,
            members,
            relays: settings.write_relays_for(persona).to_vec(),
            published_at: unix_now(),
        },
    )?;
    print_changed("follow_set.publish")
}

fn block_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: BlockInput = serde_json::from_str(input)?;
    let persona = PersonaId::parse(&input.persona_id)?;
    let settings = SettingsStore::new(root).load()?;
    let mut store = DurableStore::open(root)?;
    SocialService::new(PlatformSecretStore).set_block(
        &mut store,
        &SetBlock {
            persona_id: persona,
            target: NostrPublicKey::parse(input.target)?,
            public: input.public,
            blocked: input.blocked,
            reason: input.reason,
            relays: settings.write_relays_for(persona).to_vec(),
            changed_at: unix_now(),
        },
    )?;
    print_changed("block.set")
}

fn local_filter_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let input: LocalFilterInput = serde_json::from_str(input)?;
    let kind = match input.kind.as_str() {
        "word" => LocalFilterKind::Word,
        "topic" => LocalFilterKind::Topic,
        "thread" => LocalFilterKind::Thread,
        "media" => LocalFilterKind::Media,
        "relay" => LocalFilterKind::Relay,
        other => {
            return Err(RuntimeError::InvalidInput(format!(
                "unknown local filter kind {other}"
            )));
        }
    };
    let mut store = DurableStore::open(root)?;
    SocialService::new(PlatformSecretStore).set_local_filter(
        &mut store,
        &SetLocalFilter {
            persona_id: PersonaId::parse(&input.persona_id)?,
            kind,
            value: input.value,
            enabled: input.enabled,
            changed_at: unix_now(),
        },
    )?;
    print_changed("filter.set")
}

fn sync_action(root: &PathBuf, input: &str) -> Result<(), RuntimeError> {
    let _: serde_json::Value = serde_json::from_str(input)?;
    let mut store = DurableStore::open(root)?;
    let operation = OperationId::new();
    store.append(
        DurableEvent::OperationChanged {
            operation_id: operation,
            state: OperationState::Queued,
        },
        unix_now(),
    )?;
    let executable = env::current_exe()?;
    if let Err(error) = Command::new(executable)
        .args(["worker-sync", &operation.to_string()])
        .env("HYDRA_HOME", root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        store.append(
            DurableEvent::OperationChanged {
                operation_id: operation,
                state: OperationState::Running,
            },
            unix_now(),
        )?;
        store.append(
            DurableEvent::OperationChanged {
                operation_id: operation,
                state: OperationState::Failed,
            },
            unix_now(),
        )?;
        return Err(error.into());
    }
    print_action_result(
        "sync.now",
        operation_view(operation, OperationState::Queued, true),
        serde_json::json!({"changed": true, "accepted": 0, "failed": 0}),
    )?;
    Ok(())
}

async fn run_sync_worker(root: &PathBuf, operation: OperationId) -> Result<(), RuntimeError> {
    let mut store = DurableStore::open(root)?;
    store.append(
        DurableEvent::OperationChanged {
            operation_id: operation,
            state: OperationState::Running,
        },
        unix_now(),
    )?;
    let result = async {
        let settings = SettingsStore::new(root).load()?;
        if store.state().pending_delivery_count() > 0 {
            let relays = store
                .state()
                .outbound
                .values()
                .flat_map(|event| event.relays.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let publisher = SdkEventPublisher::new(&relays).await?;
            SyncService::sync_pending(&mut store, &publisher, unix_now()).await?;
        }
        let active_persona = active_persona_id(&store, &settings);
        let communities = subscribed_communities(&store, &settings)?;
        if !communities.is_empty() {
            let since = store
                .state()
                .heads
                .current_heads()
                .map(|head| head.edited_at)
                .max()
                .map(|latest| latest.saturating_sub(2 * 24 * 60 * 60));
            let mut relay_set = BTreeSet::new();
            if let Some(persona) = active_persona {
                relay_set.extend(allowed_read_relays(&settings, &store, persona)?);
            }
            let relays = relay_set.into_iter().collect::<Vec<_>>();
            let mut events = if relays.is_empty() {
                Vec::new()
            } else {
                hydra_nostr::fetch_community_events(&relays, &communities, since).await?
            };
            events.sort_by_key(|event| event.created_at);
            for event in events {
                ImportService::receive_public(&mut store, &event.as_json(), unix_now())?;
            }
        }
        synchronize_recent_reddit_projections(&mut store, &settings);
        let personas = store
            .state()
            .personas
            .iter()
            .map(|persona| persona.id)
            .collect::<Vec<_>>();
        for persona in personas {
            let relays = store
                .state()
                .inbox_relays
                .get(&persona)
                .cloned()
                .unwrap_or_else(|| settings.inbox_relays.clone());
            let since = private_state(&PlatformSecretStore, &store, persona)?
                .messages
                .iter()
                .map(|message| message.created_at)
                .max()
                .map(|latest| latest.saturating_sub(3 * 24 * 60 * 60));
            MessagingService::new(PlatformSecretStore)
                .receive_from_relays(&mut store, persona, &relays, since, unix_now())
                .await?;
        }
        Ok::<(), RuntimeError>(())
    }
    .await;
    let state = if result.is_ok() {
        OperationState::Succeeded
    } else {
        OperationState::Failed
    };
    store.append(
        DurableEvent::OperationChanged {
            operation_id: operation,
            state,
        },
        unix_now(),
    )?;
    result
}

fn synchronize_recent_reddit_projections(store: &mut DurableStore, settings: &Settings) {
    const ACTIVE_WINDOW: u64 = 30 * 24 * 60 * 60;
    let now = unix_now();
    let mut projections = store
        .state()
        .projections
        .values()
        .filter(|projection| {
            projection.external_id.is_some()
                && matches!(
                    projection.state,
                    hydra_domain::ProjectionState::Live
                        | hydra_domain::ProjectionState::Diverged
                        | hydra_domain::ProjectionState::Locked
                        | hydra_domain::ProjectionState::Removed
                )
                && store
                    .state()
                    .heads
                    .current_head(&projection.anchor)
                    .is_ok_and(|head| head.edited_at.saturating_add(ACTIVE_WINDOW) >= now)
        })
        .map(|projection| {
            (
                projection.id,
                projection.persona,
                projection.last_attempt_at,
            )
        })
        .collect::<Vec<_>>();
    projections.sort_by_key(|(_, _, last_attempt)| *last_attempt);
    projections.truncate(50);
    for (projection, persona, _) in projections {
        let Ok(adapter) = reddit_api(persona) else {
            continue;
        };
        let _ = BridgeService::new(PlatformSecretStore, adapter).synchronize(
            store,
            ProjectionAction {
                projection,
                relays: settings.write_relays_for(persona).to_vec(),
                recorded_at: unix_now(),
            },
        );
    }
}

fn subscribed_communities(
    store: &DurableStore,
    settings: &Settings,
) -> Result<Vec<CommunityKey>, RuntimeError> {
    let active_persona = active_persona_id(store, settings);
    let mut communities = store
        .state()
        .subscriptions
        .values()
        .filter(|item| Some(item.persona) == active_persona && item.subscribed)
        .map(|item| item.community.clone())
        .collect::<BTreeSet<_>>();
    for state in load_active_private_state(store, settings)? {
        communities.extend(
            state
                .subscriptions
                .into_values()
                .filter(|item| item.subscribed)
                .map(|item| item.community),
        );
    }
    Ok(communities.into_iter().collect())
}

fn reddit_client_id(input: Option<&str>) -> Result<String, RuntimeError> {
    input
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| env::var("HYDRA_REDDIT_CLIENT_ID").ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            RuntimeError::InvalidInput(
                "Reddit client ID is not configured; Hydra never requests a Reddit password"
                    .to_owned(),
            )
        })
}

fn reddit_api(persona: PersonaId) -> Result<RedditDataApi, RuntimeError> {
    let vault = PlatformRedditCredentialStore;
    let mut stored = vault.get(persona)?;
    let client_id = stored
        .client_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .map_or_else(|| reddit_client_id(None), Ok)?;
    if stored.expires_at <= unix_now().saturating_add(60) {
        let refresh_token = stored.refresh_token.clone().ok_or_else(|| {
            RuntimeError::InvalidInput("Reddit authorization must be renewed".to_owned())
        })?;
        let refreshed = RedditDataApi::refresh(&client_id, &refresh_token, REDDIT_USER_AGENT)?;
        stored.access_token = refreshed.access_token;
        stored.refresh_token = refreshed.refresh_token.or(Some(refresh_token));
        stored.expires_at = unix_now().saturating_add(refreshed.expires_in);
        stored.scope = refreshed.scope;
        vault.set(persona, &stored)?;
    }
    RedditDataApi::new(
        client_id,
        REDDIT_REDIRECT_URI.to_owned(),
        REDDIT_USER_AGENT.to_owned(),
        stored.access_token,
    )
    .map_err(Into::into)
}

fn credential(
    identity: hydra_reddit::RedditIdentity,
    client_id: &str,
    tokens: OAuthTokens,
    obtained_at: u64,
) -> RedditCredential {
    RedditCredential {
        identity,
        client_id: Some(client_id.to_owned()),
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: obtained_at.saturating_add(tokens.expires_in),
        scope: tokens.scope,
    }
}

fn reddit_attribution<'a>(
    kind: Option<&str>,
    link: Option<&'a str>,
) -> Result<Attribution<'a>, RuntimeError> {
    match kind.unwrap_or("none") {
        "none" => Ok(Attribution::None),
        "posted_from_hydra" => link.map(Attribution::PostedFromHydra).ok_or_else(|| {
            RuntimeError::InvalidInput("Hydra attribution requires a portable link".to_owned())
        }),
        "big_stick" => link.map(Attribution::BigStick).ok_or_else(|| {
            RuntimeError::InvalidInput("Big Stick requires a portable link".to_owned())
        }),
        other => Err(RuntimeError::InvalidInput(format!(
            "unknown Reddit attribution {other}"
        ))),
    }
}

fn resolve_projection_link(
    store: &DurableStore,
    anchor: &AnchorId,
    attribution: Option<&str>,
    supplied: Option<String>,
    relays: &[String],
) -> Result<Option<String>, RuntimeError> {
    if attribution.unwrap_or("none") == "none" {
        return Ok(None);
    }
    match supplied {
        Some(link) => Ok(Some(link)),
        None => portable_link(store, anchor, relays).map(Some),
    }
}

fn portable_link(
    store: &DurableStore,
    anchor: &AnchorId,
    relays: &[String],
) -> Result<String, RuntimeError> {
    let outbound =
        store.state().outbound.get(anchor.as_str()).ok_or_else(|| {
            RuntimeError::InvalidInput("object anchor is not available".to_owned())
        })?;
    let event = nostr::Event::from_json(&outbound.event_json)
        .map_err(|error| hydra_nostr::ProtocolError::Nostr(error.to_string()))?;
    hydra_nostr::portable_event_uri(&event, relays).map_err(Into::into)
}

fn withdrawal_marker<'a>(
    marker: &'a str,
    link: &'a str,
) -> Result<WithdrawalMarker<'a>, RuntimeError> {
    match marker {
        "reddacted" => Ok(WithdrawalMarker::Reddacted(link)),
        "withdrawn" => Ok(WithdrawalMarker::Withdrawn(link)),
        "continues" => Ok(WithdrawalMarker::Continues(link)),
        "elsewhere" => Ok(WithdrawalMarker::Elsewhere(link)),
        custom if custom.starts_with("custom:") => Ok(WithdrawalMarker::CustomLinked {
            label: custom.trim_start_matches("custom:").trim(),
            link,
        }),
        other => Err(RuntimeError::InvalidInput(format!(
            "unknown withdrawal marker {other}"
        ))),
    }
}

fn preservation_level(value: &str) -> Result<PreservationLevel, RuntimeError> {
    match value {
        "item" => Ok(PreservationLevel::Item),
        "ancestors" => Ok(PreservationLevel::Ancestors),
        "visible_siblings" => Ok(PreservationLevel::VisibleSiblings),
        "loaded_thread" => Ok(PreservationLevel::LoadedThread),
        other => Err(RuntimeError::InvalidInput(format!(
            "unknown preservation level {other}"
        ))),
    }
}

fn projection_persona(
    store: &DurableStore,
    id: hydra_domain::ProjectionId,
) -> Result<PersonaId, RuntimeError> {
    store
        .state()
        .projections
        .get(&id)
        .map(|projection| projection.persona)
        .ok_or_else(|| RuntimeError::InvalidInput("projection not found".to_owned()))
}

fn print_projection_result(
    action: &str,
    projection: &hydra_domain::Projection,
) -> Result<(), RuntimeError> {
    let mut result = serde_json::json!({
        "changed": true,
        "projectionId": projection.id.to_string(),
        "state": projection_state_name(projection.state)
    });
    if let Some(url) = &projection.external_url {
        result["externalUrl"] = serde_json::Value::String(url.clone());
    }
    print_action_result(
        action,
        operation_view(OperationId::new(), OperationState::Succeeded, false),
        result,
    )
}

fn print_changed(action: &str) -> Result<(), RuntimeError> {
    let operation = OperationId::new();
    print_action_result(
        action,
        operation_view(operation, OperationState::Succeeded, false),
        serde_json::json!({"changed": true}),
    )
}

fn print_action_result(
    action: &str,
    operation: OperationView,
    result: serde_json::Value,
) -> Result<(), RuntimeError> {
    println!(
        "{}",
        serde_json::to_string(&ActionResult {
            protocol: "theurgy-runtime-action/v1",
            app: "hydra",
            action: action.to_owned(),
            operation,
            result,
        })?
    );
    Ok(())
}

fn operation_view(id: OperationId, state: OperationState, long_running: bool) -> OperationView {
    let (status, progress) = match state {
        OperationState::Queued => ("accepted", 0),
        OperationState::Running => ("running", 50),
        OperationState::Succeeded => ("completed", 100),
        OperationState::Failed => ("failed", 100),
        OperationState::Cancelled => ("cancelled", 100),
    };
    OperationView {
        id: id.to_string(),
        status,
        progress,
        long_running,
    }
}

fn generated_at() -> String {
    format!("unix:{}", unix_now())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys, Kind, Tag, Timestamp};

    #[test]
    fn local_storage_view_only_offers_a_real_media_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();

        let initial = storage_view(root);
        assert_eq!(initial.root, root.display().to_string());
        assert_eq!(initial.media, root.join("media").display().to_string());
        assert!(!initial.media_exists);
        assert!(storage_folder(root, "media").is_err());

        fs::create_dir(root.join("media")).unwrap();
        assert!(storage_view(root).media_exists);
        assert_eq!(storage_folder(root, "data").unwrap(), root);
        assert_eq!(storage_folder(root, "media").unwrap(), root.join("media"));
        assert!(storage_folder(root, "other").is_err());
    }

    #[test]
    fn structured_local_search_preserves_explicit_scope() {
        assert_eq!(
            LocalSearchQuery::parse("/h/Science").unwrap(),
            LocalSearchQuery::Topic("science".to_owned())
        );
        assert_eq!(
            LocalSearchQuery::parse("persona:Alice").unwrap(),
            LocalSearchQuery::Persona("alice".to_owned())
        );
        assert_eq!(
            LocalSearchQuery::parse("t1_AbC123").unwrap(),
            LocalSearchQuery::Reddit("t1_abc123".to_owned())
        );
        assert!(LocalSearchQuery::parse("topic:").is_err());
    }

    #[test]
    fn reddit_search_matches_urls_and_fullnames() {
        let canonical = "https://www.reddit.com/r/hydra/comments/abc123/title/def456/";
        assert!(reddit_identifier_matches(canonical, "t1_def456"));
        assert!(reddit_identifier_matches(canonical, canonical));
        assert!(!reddit_identifier_matches(canonical, "t1_other"));
    }

    #[test]
    fn oauth_callback_requires_matching_state_and_code() {
        let request = "GET /oauth/reddit?state=expected&code=abc123 HTTP/1.1\r\n\r\n";
        assert_eq!(parse_oauth_request(request, "expected").unwrap(), "abc123");
        assert!(parse_oauth_request(request, "different").is_err());
        assert!(
            parse_oauth_request("GET /oauth/reddit?state=expected HTTP/1.1", "expected").is_err()
        );
        assert!(
            parse_oauth_request(
                "POST /oauth/reddit?state=expected&code=abc123 HTTP/1.1",
                "expected"
            )
            .is_err()
        );
        assert!(
            parse_oauth_request("GET /other?state=expected&code=abc123 HTTP/1.1", "expected")
                .is_err()
        );
        assert!(
            parse_oauth_request(
                "GET /oauth/reddit?state=expected&state=expected&code=abc123 HTTP/1.1",
                "expected"
            )
            .is_err()
        );
    }

    #[test]
    fn reddit_user_agent_is_versioned_and_identifies_the_developer() {
        assert_eq!(
            REDDIT_USER_AGENT,
            "desktop:io.hydra.Hydra:1.0.0 (by /u/raisondecalcul)"
        );
    }

    #[test]
    fn extension_urls_resolve_exact_reddit_objects() {
        let post = Url::parse("https://www.reddit.com/r/hydra/comments/abc123/title/").unwrap();
        let comment =
            Url::parse("https://old.reddit.com/r/hydra/comments/abc123/title/def456/").unwrap();
        assert_eq!(
            reddit_fullname_from_url(&post).unwrap().as_str(),
            "t3_abc123"
        );
        assert_eq!(
            reddit_fullname_from_url(&comment).unwrap().as_str(),
            "t1_def456"
        );
        assert!(
            reddit_fullname_from_url(&Url::parse("https://www.reddit.com/r/hydra/").unwrap())
                .is_err()
        );
    }

    #[test]
    fn desktop_host_bounds_and_recovers_after_hostile_lines() {
        let mut input = vec![b'x'; 33];
        input.extend_from_slice(b"\n{}\n");
        let mut cursor = std::io::Cursor::new(input);
        assert!(matches!(
            read_desktop_line(&mut cursor, 32).unwrap(),
            Some(DesktopLine::Invalid("desktop request exceeds 1 MiB"))
        ));
        assert!(matches!(
            read_desktop_line(&mut cursor, 32).unwrap(),
            Some(DesktopLine::Request(request)) if request == "{}"
        ));
        assert!(read_desktop_line(&mut cursor, 32).unwrap().is_none());

        let mut invalid_utf8 = std::io::Cursor::new(vec![0xff, b'\n', b'{', b'}', b'\n']);
        assert!(matches!(
            read_desktop_line(&mut invalid_utf8, 32).unwrap(),
            Some(DesktopLine::Invalid("desktop request is not UTF-8"))
        ));
        assert!(matches!(
            read_desktop_line(&mut invalid_utf8, 32).unwrap(),
            Some(DesktopLine::Request(request)) if request == "{}"
        ));
    }

    #[test]
    fn open_nostr_page_is_globally_bounded_after_relay_merge() {
        let keys = Keys::generate();
        let newest = EventBuilder::new(Kind::TextNote, "newest")
            .custom_created_at(Timestamp::from(30))
            .sign_with_keys(&keys)
            .unwrap();
        let middle = EventBuilder::new(Kind::TextNote, "middle")
            .custom_created_at(Timestamp::from(20))
            .sign_with_keys(&keys)
            .unwrap();
        let oldest = EventBuilder::new(Kind::TextNote, "oldest")
            .custom_created_at(Timestamp::from(10))
            .sign_with_keys(&keys)
            .unwrap();
        let events = bounded_open_page(vec![oldest, middle.clone(), newest, middle], 2);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0.content, "newest");
        assert_eq!(events[1].0.content, "middle");
    }

    #[test]
    fn open_nostr_media_has_a_useful_textual_fallback() {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(20), "")
            .tags([Tag::parse([
                "imeta",
                "url https://media.example/picture.jpg",
                "alt A moonlit gathering",
            ])
            .unwrap()])
            .sign_with_keys(&keys)
            .unwrap();

        assert_eq!(
            open_event_body(&event).as_deref(),
            Some("A moonlit gathering")
        );
    }

    #[test]
    fn open_nostr_page_omits_events_with_no_visible_content() {
        let keys = Keys::generate();
        let empty = EventBuilder::new(Kind::Custom(20), "")
            .sign_with_keys(&keys)
            .unwrap();
        let visible = EventBuilder::new(Kind::TextNote, "visible")
            .sign_with_keys(&keys)
            .unwrap();

        let events = bounded_open_page(vec![empty, visible], 30);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1, "visible");
    }
}
