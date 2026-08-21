use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    ArchiveManifest, BlockRecord, CommunitySubscription, ContinuityWorkflow, DeliveryState,
    EncryptedPrivateRecord, FlockingJudgmentRecord, FollowRecord, MediaManifest, ObjectHead,
    OperationId, OperationState, OutboundEvent, Persona, Projection, PublicFollowSet,
    PublicProjectionRecord, ReactionRecord,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(Uuid);

impl EventId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

/// The exhaustive durable facts understood by the current core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum DurableEvent {
    PersonaCreated(Persona),
    PersonaUpdated(Persona),
    PersonaProfilePublished {
        persona: crate::PersonaId,
        display_name: String,
        outbound: OutboundEvent,
    },
    RedditIdentityProofPublished {
        proof: crate::RedditIdentityProof,
        outbound: OutboundEvent,
    },
    InboxRelaysChanged {
        persona: crate::PersonaId,
        relays: Vec<String>,
        outbound: OutboundEvent,
    },
    PersonaRelaysChanged {
        persona: crate::PersonaId,
        read: Vec<String>,
        write: Vec<String>,
        outbound: OutboundEvent,
    },
    NativeObjectChanged {
        head: ObjectHead,
        outbound: Vec<OutboundEvent>,
    },
    PublicEventQueued {
        persona: crate::PersonaId,
        outbound: OutboundEvent,
    },
    ObjectDisowningRequested {
        persona: crate::PersonaId,
        anchor: crate::AnchorId,
        reason: String,
        outbound: OutboundEvent,
    },
    RemoteEventReceived {
        event_id: String,
        event_json: String,
        heads: Vec<ObjectHead>,
        #[serde(default)]
        reactions: Vec<ReactionRecord>,
        #[serde(default)]
        public_projections: Vec<PublicProjectionRecord>,
        #[serde(default)]
        flocking_judgments: Vec<flocking_core::Judgment>,
        #[serde(default)]
        community_appearances: Vec<flocking_core::CommunityAppearance>,
    },
    MediaPreserved(MediaManifest),
    MediaPreservedFor {
        persona: crate::PersonaId,
        manifest: MediaManifest,
    },
    MediaPublished {
        persona: crate::PersonaId,
        manifest: MediaManifest,
        outbound: OutboundEvent,
    },
    ArchiveCaptured(ArchiveManifest),
    ProjectionChanged {
        projection: Projection,
        outbound: Option<OutboundEvent>,
    },
    DeliveryRecorded {
        event_id: String,
        relay: String,
        state: DeliveryState,
    },
    ReactionRecorded {
        reaction: ReactionRecord,
        outbound: OutboundEvent,
    },
    PrivateRecordStored {
        record: EncryptedPrivateRecord,
        public_follow_retraction: Option<FollowRecord>,
        public_block_retraction: Option<BlockRecord>,
        public_subscription_retraction: Option<CommunitySubscription>,
        outbound: Vec<OutboundEvent>,
    },
    FollowChanged {
        follow: FollowRecord,
        outbound: Vec<OutboundEvent>,
    },
    PublicFollowSetPublished {
        set: PublicFollowSet,
        outbound: OutboundEvent,
    },
    CommunitySubscriptionChanged {
        subscription: CommunitySubscription,
        outbound: Vec<OutboundEvent>,
    },
    BlockChanged {
        block: BlockRecord,
        outbound: Vec<OutboundEvent>,
    },
    FlockingJudgmentChanged {
        record: FlockingJudgmentRecord,
        outbound: Vec<OutboundEvent>,
    },
    OperationChanged {
        operation_id: OperationId,
        state: OperationState,
    },
    ContinuityWorkflowChanged(ContinuityWorkflow),
}

/// Versioned append-only record. `checksum` is calculated by the store over the
/// canonical serialized payload and preceding record checksum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub schema: String,
    pub id: EventId,
    pub recorded_at: u64,
    pub previous_checksum: Option<String>,
    pub event: DurableEvent,
    pub checksum: String,
}

impl EventEnvelope {
    pub const SCHEMA: &'static str = "hydra-event/v1";
}

impl DurableEvent {
    /// Revalidates all bounded fields before an event is appended or replayed.
    ///
    /// This is deliberately independent of writer-side checks so a migrated,
    /// imported, or hand-edited log cannot bypass the domain contracts.
    ///
    /// # Errors
    ///
    /// Returns an error when any durable component is malformed or unbounded.
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive match keeps every durable event variant visibly covered"
    )]
    pub fn validate(&self) -> Result<(), crate::DomainError> {
        match self {
            Self::PersonaCreated(persona) | Self::PersonaUpdated(persona) => persona.validate(),
            Self::PersonaProfilePublished {
                display_name,
                outbound,
                ..
            } => {
                if display_name.trim().is_empty()
                    || display_name.len() > crate::Persona::MAX_DISPLAY_NAME_LEN
                    || crate::text::has_unsafe_inline_text(display_name)
                {
                    return Err(crate::DomainError::InvalidObjectShape);
                }
                outbound.validate()
            }
            Self::RedditIdentityProofPublished { proof, outbound } => {
                proof.validate()?;
                outbound.validate()
            }
            Self::InboxRelaysChanged {
                relays, outbound, ..
            } => {
                validate_relays(relays, 3)?;
                outbound.validate()
            }
            Self::PersonaRelaysChanged {
                read,
                write,
                outbound,
                ..
            } => {
                validate_relays(read, crate::OutboundEvent::MAX_RELAYS)?;
                validate_relays(write, crate::OutboundEvent::MAX_RELAYS)?;
                outbound.validate()
            }
            Self::NativeObjectChanged { head, outbound } => {
                head.validate()?;
                validate_outbound(outbound)
            }
            Self::ObjectDisowningRequested {
                anchor,
                reason,
                outbound,
                ..
            } => {
                crate::AnchorId::parse(anchor.as_str())?;
                if reason.len() > 500 || reason.chars().any(char::is_control) {
                    return Err(crate::DomainError::InvalidObjectShape);
                }
                outbound.validate()
            }
            Self::RemoteEventReceived {
                event_id,
                event_json,
                heads,
                reactions,
                public_projections,
                flocking_judgments,
                community_appearances,
            } => {
                if event_id.is_empty()
                    || event_id.len() > 128
                    || event_id.chars().any(char::is_whitespace)
                    || event_json.is_empty()
                    || event_json.len() > crate::OutboundEvent::MAX_EVENT_BYTES
                    || heads.len() > 8
                    || reactions.len() > 8
                    || public_projections.len() > 8
                    || flocking_judgments.len() > 512
                    || community_appearances.len() > 8
                {
                    return Err(crate::DomainError::InvalidObjectShape);
                }
                for head in heads {
                    head.validate()?;
                }
                for reaction in reactions {
                    reaction.validate()?;
                }
                for appearance in community_appearances {
                    appearance
                        .validate()
                        .map_err(|_| crate::DomainError::InvalidFlocking)?;
                }
                for projection in public_projections {
                    projection.validate()?;
                }
                for judgment in flocking_judgments {
                    judgment
                        .validate()
                        .map_err(|_| crate::DomainError::InvalidFlocking)?;
                }
                Ok(())
            }
            Self::PublicEventQueued { outbound, .. } => outbound.validate(),
            Self::MediaPreserved(manifest) | Self::MediaPreservedFor { manifest, .. } => {
                manifest.validate()
            }
            Self::MediaPublished {
                manifest, outbound, ..
            } => {
                manifest.validate()?;
                outbound.validate()
            }
            Self::ArchiveCaptured(manifest) => manifest.validate(),
            Self::ProjectionChanged {
                projection,
                outbound,
            } => {
                projection.validate()?;
                outbound
                    .as_ref()
                    .map(crate::OutboundEvent::validate)
                    .transpose()
                    .map(|_| ())
            }
            Self::DeliveryRecorded {
                event_id,
                relay,
                state,
            } => {
                if event_id.is_empty()
                    || event_id.len() > 128
                    || !crate::transport::valid_relay(relay)
                    || match state {
                        crate::DeliveryState::Accepted => false,
                        crate::DeliveryState::Rejected { reason }
                        | crate::DeliveryState::Failed { reason } => {
                            reason.len() > 2_048 || reason.chars().any(char::is_control)
                        }
                    }
                {
                    return Err(crate::DomainError::InvalidObjectShape);
                }
                Ok(())
            }
            Self::ReactionRecorded { reaction, outbound } => {
                reaction.validate()?;
                outbound.validate()
            }
            Self::PrivateRecordStored {
                record,
                public_follow_retraction,
                public_block_retraction,
                public_subscription_retraction,
                outbound,
            } => {
                record.validate()?;
                if let Some(follow) = public_follow_retraction {
                    crate::NostrPublicKey::parse(follow.target.as_str().to_owned())?;
                }
                if let Some(block) = public_block_retraction {
                    crate::NostrPublicKey::parse(block.target.as_str().to_owned())?;
                    block.validate()?;
                }
                if let Some(subscription) = public_subscription_retraction {
                    crate::CommunityKey::parse(subscription.community.as_str())?;
                }
                validate_outbound(outbound)
            }
            Self::FollowChanged { follow, outbound } => {
                crate::NostrPublicKey::parse(follow.target.as_str().to_owned())?;
                validate_outbound(outbound)
            }
            Self::PublicFollowSetPublished { set, outbound } => {
                set.validate()?;
                outbound.validate()
            }
            Self::CommunitySubscriptionChanged { outbound, .. } => validate_outbound(outbound),
            Self::BlockChanged { block, outbound } => {
                crate::NostrPublicKey::parse(block.target.as_str().to_owned())?;
                block.validate()?;
                validate_outbound(outbound)
            }
            Self::FlockingJudgmentChanged { record, outbound } => {
                record.validate()?;
                if !record.public {
                    return Err(crate::DomainError::InvalidFlocking);
                }
                validate_outbound(outbound)
            }
            Self::OperationChanged { .. } | Self::ContinuityWorkflowChanged(_) => Ok(()),
        }
    }
}

fn validate_outbound(events: &[crate::OutboundEvent]) -> Result<(), crate::DomainError> {
    if events.len() > 8 {
        return Err(crate::DomainError::InvalidObjectShape);
    }
    for event in events {
        event.validate()?;
    }
    Ok(())
}

fn validate_relays(relays: &[String], maximum: usize) -> Result<(), crate::DomainError> {
    if relays.is_empty()
        || relays.len() > maximum
        || relays
            .iter()
            .any(|relay| !crate::transport::valid_relay(relay))
        || relays
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != relays.len()
    {
        return Err(crate::DomainError::InvalidObjectShape);
    }
    Ok(())
}
