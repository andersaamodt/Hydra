use std::collections::BTreeMap;

use flocking_core::{Config as FlockingConfig, Judgment, SourceState};
use serde::{Deserialize, Serialize};

use crate::{AnchorId, CommunityKey, DomainError, NostrPublicKey, PersonaId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftKind {
    Post,
    Comment,
    Norm,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftRecord {
    pub id: String,
    pub persona: PersonaId,
    pub kind: DraftKind,
    pub title: Option<String>,
    pub body: String,
    pub communities: Vec<CommunityKey>,
    pub parent: Option<AnchorId>,
    pub updated_at: u64,
    pub discarded: bool,
}

impl DraftRecord {
    pub const MAX_ID_LEN: usize = 128;
    /// Validates persona-owned composer state before it is encrypted.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty identifier or an invalid shape for the
    /// selected draft kind.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.id.trim().is_empty() {
            return Err(DomainError::Empty);
        }
        if self.id.len() > Self::MAX_ID_LEN
            || self.id.chars().any(char::is_control)
            || self.body.len() > crate::ContentBody::MAX_LEN
            || self.title.as_ref().is_some_and(|title| {
                title.len() > crate::ObjectHead::MAX_TITLE_LEN
                    || crate::text::has_unsafe_inline_text(title)
            })
            || self.communities.len() > crate::ObjectHead::MAX_COMMUNITIES
        {
            return Err(DomainError::InvalidObjectShape);
        }
        match self.kind {
            DraftKind::Post if self.communities.is_empty() || self.parent.is_some() => {
                Err(DomainError::InvalidObjectShape)
            }
            DraftKind::Comment if self.parent.is_none() || !self.communities.is_empty() => {
                Err(DomainError::InvalidObjectShape)
            }
            DraftKind::Norm if self.communities.len() != 1 || self.parent.is_some() => {
                Err(DomainError::InvalidObjectShape)
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionValue {
    Upvote,
    Downvote,
    Neutral,
    Emoji(String),
}

impl ReactionValue {
    pub const MAX_EMOJI_LEN: usize = 32;

    /// Validates a reaction and returns its NIP-25 content representation.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty or unreasonably large custom reaction.
    pub fn wire_value(&self) -> Result<&str, DomainError> {
        match self {
            Self::Upvote => Ok("+"),
            Self::Downvote => Ok("-"),
            Self::Neutral => Ok("0"),
            Self::Emoji(value) if value.trim().is_empty() => Err(DomainError::InvalidReaction),
            Self::Emoji(value) if value.len() > Self::MAX_EMOJI_LEN => Err(DomainError::TooLong {
                max: Self::MAX_EMOJI_LEN,
            }),
            Self::Emoji(value) => Ok(value),
        }
    }

    #[must_use]
    pub const fn is_stance(&self) -> bool {
        matches!(self, Self::Upvote | Self::Downvote | Self::Neutral)
    }

    #[must_use]
    pub const fn can_reaffirm(&self) -> bool {
        matches!(self, Self::Upvote | Self::Downvote)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionRecord {
    pub actor: NostrPublicKey,
    pub target: AnchorId,
    pub value: ReactionValue,
    pub occurred_at: u64,
    pub credited_reaffirmation: bool,
    pub source_event_id: String,
}

impl ReactionRecord {
    /// Revalidates a reaction received from the network or durable log.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed identity, target, value, or source ID.
    pub fn validate(&self) -> Result<(), DomainError> {
        NostrPublicKey::parse(self.actor.as_str().to_owned())?;
        AnchorId::parse(self.target.as_str())?;
        self.value.wire_value()?;
        if self.credited_reaffirmation && !self.value.can_reaffirm() {
            return Err(DomainError::InvalidReaction);
        }
        if self.source_event_id.trim().is_empty()
            || self.source_event_id.len() > 128
            || self.source_event_id.chars().any(char::is_whitespace)
        {
            return Err(DomainError::InvalidReaction);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisitIntent {
    ReturnSoon,
    ReconsiderVote,
    ReviewOnDate,
    Study,
    NotifyOnActivity,
    Collection(String),
}

impl RevisitIntent {
    pub const MAX_COLLECTION_LEN: usize = 100;

    /// Validates free-form collection names.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty collection name.
    pub fn validate(&self) -> Result<(), DomainError> {
        if let Self::Collection(value) = self {
            if value.trim().is_empty() {
                return Err(DomainError::Empty);
            }
            if value.len() > Self::MAX_COLLECTION_LEN || value.chars().any(char::is_control) {
                return Err(DomainError::InvalidObjectShape);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisitRecord {
    pub persona: PersonaId,
    pub target: AnchorId,
    pub intent: RevisitIntent,
    pub due_at: Option<u64>,
    #[serde(default = "default_active")]
    pub active: bool,
}

impl RevisitRecord {
    /// Validates the intent-specific fields of one private memory record.
    ///
    /// # Errors
    ///
    /// Returns an error when a dated reminder has no date.
    pub fn validate(&self) -> Result<(), DomainError> {
        self.intent.validate()?;
        if matches!(self.intent, RevisitIntent::ReviewOnDate) && self.due_at.is_none() {
            return Err(DomainError::InvalidRevisit);
        }
        Ok(())
    }
}

const fn default_active() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowRecord {
    pub persona: PersonaId,
    pub target: NostrPublicKey,
    pub public: bool,
    pub following: bool,
    pub changed_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicFollowSet {
    pub persona: PersonaId,
    pub identifier: String,
    pub title: String,
    pub members: Vec<NostrPublicKey>,
    pub published_at: u64,
}

impl PublicFollowSet {
    /// Validates a standard public NIP-51 follow set.
    ///
    /// # Errors
    ///
    /// Returns an error for empty metadata, duplicate/empty membership, or
    /// values too large for an interoperable list event.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.identifier.trim().is_empty() || self.title.trim().is_empty() {
            return Err(DomainError::Empty);
        }
        if self.identifier.len() > 100 || self.title.len() > 200 || self.members.len() > 500 {
            return Err(DomainError::TooLong { max: 500 });
        }
        if self.members.is_empty()
            || self
                .members
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.members.len()
        {
            return Err(DomainError::InvalidObjectShape);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommunitySubscription {
    pub persona: PersonaId,
    pub community: crate::CommunityKey,
    pub public: bool,
    pub subscribed: bool,
    pub changed_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRecord {
    pub persona: PersonaId,
    pub target: NostrPublicKey,
    pub public: bool,
    pub blocked: bool,
    pub reason: Option<String>,
    pub changed_at: u64,
}

/// Encrypted persona-local configuration for voluntarily followed judgments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlockingProfile {
    pub persona: PersonaId,
    pub config: FlockingConfig,
    pub source_states: Vec<SourceState>,
    pub changed_at: u64,
}

impl FlockingProfile {
    /// Revalidates the portable configuration and its declared source inputs.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid configuration or a source state outside
    /// one of its grants.
    pub fn validate(&self) -> Result<(), DomainError> {
        self.config
            .validate()
            .map_err(|_| DomainError::InvalidFlocking)?;
        for state in &self.source_states {
            let Some(grant) = self.config.grant(&state.source, state.faculty) else {
                return Err(DomainError::InvalidFlocking);
            };
            if !grant.enables(&state.scope) {
                return Err(DomainError::InvalidFlocking);
            }
        }
        Ok(())
    }
}

/// One direct Flocking judgment authored locally or queued for publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlockingJudgmentRecord {
    pub persona: PersonaId,
    pub public: bool,
    pub judgment: Judgment,
}

impl FlockingJudgmentRecord {
    /// Revalidates one direct judgment before it affects a persona's view.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid Flocking tuple or action.
    pub fn validate(&self) -> Result<(), DomainError> {
        self.judgment
            .validate()
            .map_err(|_| DomainError::InvalidFlocking)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalFilterKind {
    Word,
    Topic,
    Thread,
    Media,
    Relay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalFilterRecord {
    pub persona: PersonaId,
    pub kind: LocalFilterKind,
    pub value: String,
    pub enabled: bool,
    pub changed_at: u64,
}

impl LocalFilterRecord {
    /// Validates one encrypted local filter without interpreting its meaning.
    ///
    /// # Errors
    ///
    /// Returns an error for empty or excessively large filter values.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.value.trim().is_empty() {
            return Err(DomainError::Empty);
        }
        if self.value.len() > 512 {
            return Err(DomainError::TooLong { max: 512 });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageDirection {
    Sent,
    Received,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectMessageRecord {
    pub persona: PersonaId,
    pub peer: NostrPublicKey,
    pub direction: MessageDirection,
    pub body: String,
    pub created_at: u64,
    pub rumor_id: String,
    pub request: bool,
}

impl DirectMessageRecord {
    pub const MAX_BODY_LEN: usize = crate::ContentBody::MAX_LEN;
    pub const MAX_RUMOR_ID_LEN: usize = 128;

    /// Validates bounded private-message state after creation or decryption.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty/oversized body or malformed rumor ID.
    pub fn validate(&self) -> Result<(), DomainError> {
        crate::ContentBody::parse(self.body.clone())?;
        NostrPublicKey::parse(self.peer.as_str().to_owned())?;
        if self.rumor_id.trim().is_empty()
            || self.rumor_id.len() > Self::MAX_RUMOR_ID_LEN
            || self.rumor_id.chars().any(char::is_whitespace)
        {
            return Err(DomainError::InvalidObjectShape);
        }
        Ok(())
    }
}

/// A local social or memory fact that must not be readable in the durable log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum PrivateRecord {
    Draft(DraftRecord),
    Revisit(RevisitRecord),
    Follow(FollowRecord),
    Block(BlockRecord),
    DirectMessage(DirectMessageRecord),
    CommunitySubscription(CommunitySubscription),
    LocalFilter(LocalFilterRecord),
    FlockingProfile(FlockingProfile),
    FlockingJudgment(FlockingJudgmentRecord),
}

impl PrivateRecord {
    /// Validates decrypted persona-local state before it affects a materialized
    /// view.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed or unbounded private payloads.
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Draft(item) => item.validate(),
            Self::Revisit(item) => {
                AnchorId::parse(item.target.as_str())?;
                item.validate()
            }
            Self::Follow(item) => {
                NostrPublicKey::parse(item.target.as_str().to_owned()).map(|_| ())
            }
            Self::Block(item) => {
                NostrPublicKey::parse(item.target.as_str().to_owned())?;
                item.validate()
            }
            Self::DirectMessage(item) => item.validate(),
            Self::CommunitySubscription(_) => Ok(()),
            Self::LocalFilter(item) => item.validate(),
            Self::FlockingProfile(item) => item.validate(),
            Self::FlockingJudgment(item) => item.validate(),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PrivateState {
    pub drafts: BTreeMap<String, DraftRecord>,
    pub revisits: BTreeMap<AnchorId, RevisitRecord>,
    pub follows: BTreeMap<NostrPublicKey, FollowRecord>,
    pub blocks: BTreeMap<NostrPublicKey, BlockRecord>,
    pub messages: Vec<DirectMessageRecord>,
    pub subscriptions: BTreeMap<crate::CommunityKey, CommunitySubscription>,
    pub filters: BTreeMap<(LocalFilterKind, String), LocalFilterRecord>,
    pub flocking_profile: Option<FlockingProfile>,
    pub flocking_judgments: BTreeMap<String, FlockingJudgmentRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedPrivateRecord {
    pub persona: PersonaId,
    pub ciphertext: String,
    pub stored_at: u64,
    pub source_event_id: Option<String>,
}

impl EncryptedPrivateRecord {
    pub const MAX_CIPHERTEXT_LEN: usize = 2_097_152;

    /// Validates the bounded encrypted envelope before retaining it.
    ///
    /// # Errors
    ///
    /// Returns an error for empty or oversized ciphertext or source metadata.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.ciphertext.is_empty() {
            return Err(DomainError::Empty);
        }
        if self.ciphertext.len() > Self::MAX_CIPHERTEXT_LEN
            || self
                .source_event_id
                .as_ref()
                .is_some_and(|value| value.len() > 128 || value.chars().any(char::is_whitespace))
        {
            return Err(DomainError::InvalidObjectShape);
        }
        Ok(())
    }
}

impl BlockRecord {
    pub const MAX_REASON_LEN: usize = 500;

    /// Validates an optional public claim without interpreting it as judgment.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty or oversized supplied reason.
    pub fn validate(&self) -> Result<(), DomainError> {
        if let Some(reason) = &self.reason {
            if reason.trim().is_empty() {
                return Err(DomainError::Empty);
            }
            if reason.len() > Self::MAX_REASON_LEN {
                return Err(DomainError::TooLong {
                    max: Self::MAX_REASON_LEN,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduled_revisit_requires_a_date() {
        let mut revisit = RevisitRecord {
            persona: PersonaId::new(),
            target: AnchorId::parse("0".repeat(64)).unwrap(),
            intent: RevisitIntent::ReviewOnDate,
            due_at: None,
            active: true,
        };
        assert_eq!(revisit.validate(), Err(DomainError::InvalidRevisit));
        revisit.due_at = Some(42);
        assert_eq!(revisit.validate(), Ok(()));
    }

    #[test]
    fn public_follow_sets_require_explicit_unique_members() {
        let member = NostrPublicKey::parse("npub-member").unwrap();
        let mut set = PublicFollowSet {
            persona: PersonaId::new(),
            identifier: "recommended".to_owned(),
            title: "Recommended personas".to_owned(),
            members: vec![member.clone()],
            published_at: 42,
        };
        assert_eq!(set.validate(), Ok(()));
        set.members.push(member);
        assert_eq!(set.validate(), Err(DomainError::InvalidObjectShape));
    }
}
