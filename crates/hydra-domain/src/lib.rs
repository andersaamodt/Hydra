#![forbid(unsafe_code)]
//! Framework-free Hydra domain model.

mod community;
mod content;
mod continuity;
mod error;
mod event;
mod identity;
mod media;
mod operation;
mod projection;
mod publication;
mod social;
mod text;
mod theme;
mod transport;

pub use community::CommunityKey;
pub use content::{
    AnchorId, ArchiveId, ArchiveManifest, ContentBody, ExternalId, ObjectHead, ObjectKind,
    PreservationLevel,
};
pub use continuity::{
    BigStickState, ContinuityState, ContinuityWorkflow, PersonaSwitchState, ReddactedState,
};
pub use error::DomainError;
pub use event::{DurableEvent, EventEnvelope, EventId};
pub use identity::{
    NostrPublicKey, Persona, PersonaId, PersonaRegistry, RedditAccountId, RedditIdentityProof,
};
pub use media::MediaManifest;
pub use operation::{OperationId, OperationState};
pub use projection::{Projection, ProjectionId, ProjectionState, PublicProjectionRecord};
pub use publication::{PublicationState, ReplicationStatus};
pub use social::{
    BlockRecord, CommunityAppearanceRecord, CommunityColorChoiceRecord, CommunitySubscription,
    DirectMessageRecord, DraftKind, DraftRecord, EncryptedPrivateRecord, FlockingJudgmentRecord,
    FlockingProfile, FollowRecord, LocalFilterKind, LocalFilterRecord, MessageDirection,
    PrivateRecord, PrivateState, PublicFollowSet, ReactionRecord, ReactionValue, RevisitIntent,
    RevisitRecord,
};
pub use theme::{
    COMMUNITY_COLOR_SCHEME_VERSION, CommunityColorChoice, CommunityColorInput,
    CommunityColorResult, CommunityColorScheme, evaluate_community_colors,
};
pub use transport::{DeliveryState, OutboundEvent};
