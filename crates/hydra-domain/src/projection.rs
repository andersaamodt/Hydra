use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

use crate::NostrPublicKey;
use crate::{AnchorId, DomainError, ExternalId, PersonaId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectionId(Uuid);

impl ProjectionId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parses a durable projection identifier.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid UUID.
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        Uuid::parse_str(value)
            .map(Self)
            .map_err(|_| DomainError::InvalidProjectionId)
    }
}

impl Default for ProjectionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ProjectionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectionState {
    NotRequested,
    Queued,
    Submitting,
    #[serde(alias = "Projected")]
    Live,
    Synchronizing,
    Diverged,
    Locked,
    Removed,
    Deleted,
    Rejected,
    Withdrawn,
    Failed,
    Abandoned,
}

impl ProjectionState {
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        use ProjectionState::{
            Abandoned, Deleted, Diverged, Failed, Live, Locked, NotRequested, Queued, Rejected,
            Removed, Submitting, Synchronizing, Withdrawn,
        };
        matches!(
            (self, next),
            (
                NotRequested | Failed | Rejected | Abandoned,
                Queued | Abandoned
            ) | (Queued, Submitting | Failed | Abandoned)
                | (Submitting, Live | Rejected | Failed | Abandoned)
                | (
                    Live,
                    Synchronizing
                        | Diverged
                        | Locked
                        | Removed
                        | Deleted
                        | Withdrawn
                        | Failed
                        | Abandoned
                )
                | (
                    Synchronizing,
                    Live | Diverged | Locked | Removed | Deleted | Failed | Abandoned
                )
                | (
                    Diverged | Locked | Removed,
                    Synchronizing | Withdrawn | Abandoned
                )
                | (Deleted, Withdrawn | Abandoned)
        ) || self == next
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projection {
    pub id: ProjectionId,
    pub anchor: AnchorId,
    /// Adapter-owned destination, such as a community or exact parent object.
    pub destination: ExternalId,
    pub external_id: Option<ExternalId>,
    pub external_url: Option<String>,
    pub persona: PersonaId,
    pub state: ProjectionState,
    #[serde(default = "default_sync_enabled")]
    pub sync_enabled: bool,
    pub payload_hash: Option<String>,
    pub last_synced_head: Option<String>,
    pub rendered_payload: Option<String>,
    /// Adapter-rendered suffix that must survive canonical Hydra edits.
    #[serde(default)]
    pub rendered_suffix: Option<String>,
    pub formatting_losses: Vec<String>,
    pub last_attempt_at: Option<u64>,
    pub last_success_at: Option<u64>,
    pub divergence: Option<String>,
    pub display_error: Option<String>,
}

/// Public, cross-client evidence that one Hydra object has a Reddit
/// manifestation. Adapter credentials, retry details, payloads, and errors
/// deliberately remain private local state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicProjectionRecord {
    pub source_event_id: String,
    pub address: String,
    pub author: NostrPublicKey,
    pub anchor: AnchorId,
    pub external_id: ExternalId,
    pub external_url: String,
    pub reddit_fullname: String,
    pub target_subreddit: String,
    pub projection_type: String,
    pub state: String,
    pub current_head: Option<String>,
    pub recorded_at: u64,
}

impl PublicProjectionRecord {
    /// Revalidates a received record independently of its wire parser.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed or unbounded public projection data.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.source_event_id.len() != 64
            || !self
                .source_event_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || !self.address.starts_with("hydra:projection:")
            || self.address.len() != "hydra:projection:".len() + 64
            || !self.address["hydra:projection:".len()..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self.external_url.len() > ExternalId::MAX_CANONICAL_LEN
            || !self.external_url.starts_with("https://")
            || self.external_url.chars().any(char::is_whitespace)
            || self.reddit_fullname != self.external_id.canonical
            || !matches!(self.external_id.system.as_str(), "reddit")
            || !matches!(self.projection_type.as_str(), "post" | "comment")
            || !matches!(
                (self.projection_type.as_str(), self.reddit_fullname.get(..3)),
                ("post", Some("t3_")) | ("comment", Some("t1_"))
            )
            || self.target_subreddit.is_empty()
            || self.target_subreddit.len() > 64
            || !matches!(
                self.state.as_str(),
                "live" | "locked" | "removed" | "deleted" | "withdrawn"
            )
            || self
                .current_head
                .as_ref()
                .is_some_and(|value| value.len() > AnchorId::MAX_LEN)
        {
            return Err(DomainError::InvalidObjectShape);
        }
        AnchorId::parse(self.anchor.as_str())?;
        self.external_id.validate()?;
        Ok(())
    }
}

const fn default_sync_enabled() -> bool {
    true
}

impl Projection {
    pub const MAX_RENDERED_PAYLOAD_LEN: usize = 200_000;
    pub const MAX_FORMATTING_LOSSES: usize = 32;

    /// Validates bounded adapter state after deserialization.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe identifiers, URLs, hashes, or diagnostic
    /// payloads that exceed the local journal's public contract.
    pub fn validate(&self) -> Result<(), DomainError> {
        AnchorId::parse(self.anchor.as_str())?;
        self.destination.validate()?;
        self.external_id
            .as_ref()
            .map(ExternalId::validate)
            .transpose()?;
        if self.external_url.as_ref().is_some_and(|value| {
            value.len() > ExternalId::MAX_CANONICAL_LEN
                || !value.starts_with("https://")
                || value.chars().any(char::is_whitespace)
        }) || self.payload_hash.as_ref().is_some_and(|value| {
            value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) || self
            .last_synced_head
            .as_ref()
            .is_some_and(|value| value.len() > AnchorId::MAX_LEN)
            || self
                .rendered_payload
                .as_ref()
                .is_some_and(|value| value.len() > Self::MAX_RENDERED_PAYLOAD_LEN)
            || self
                .rendered_suffix
                .as_ref()
                .is_some_and(|value| value.len() > 4_096)
            || self.formatting_losses.len() > Self::MAX_FORMATTING_LOSSES
            || self
                .formatting_losses
                .iter()
                .any(|value| value.len() > 500 || value.chars().any(char::is_control))
            || self
                .divergence
                .as_ref()
                .is_some_and(|value| value.len() > 2_048)
            || self
                .display_error
                .as_ref()
                .is_some_and(|value| value.len() > 2_048)
        {
            return Err(DomainError::InvalidObjectShape);
        }
        Ok(())
    }

    /// Applies one legal projection-state transition.
    ///
    /// # Errors
    ///
    /// Returns an error when the requested transition is not in the authoritative
    /// projection state machine.
    pub fn transition(&mut self, next: ProjectionState) -> Result<(), DomainError> {
        if !self.state.can_transition_to(next) {
            return Err(DomainError::InvalidTransition {
                from: format!("{:?}", self.state),
                to: format!("{next:?}"),
            });
        }
        self.state = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projection() -> Projection {
        Projection {
            id: ProjectionId::new(),
            anchor: AnchorId::parse("anchor").unwrap(),
            destination: ExternalId::new("example", "destination").unwrap(),
            external_id: None,
            external_url: None,
            persona: PersonaId::new(),
            state: ProjectionState::NotRequested,
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
        }
    }

    #[test]
    fn follows_the_happy_path() {
        let mut projection = projection();
        for state in [
            ProjectionState::Queued,
            ProjectionState::Submitting,
            ProjectionState::Live,
            ProjectionState::Synchronizing,
            ProjectionState::Live,
        ] {
            projection.transition(state).unwrap();
        }
    }

    #[test]
    fn cannot_skip_submission() {
        let mut projection = projection();
        assert!(projection.transition(ProjectionState::Live).is_err());
    }

    #[test]
    fn withdrawn_content_is_terminal() {
        let mut projection = projection();
        projection.transition(ProjectionState::Queued).unwrap();
        projection.transition(ProjectionState::Submitting).unwrap();
        projection.transition(ProjectionState::Live).unwrap();
        projection.transition(ProjectionState::Withdrawn).unwrap();
        assert!(projection.transition(ProjectionState::Live).is_err());
        assert!(projection.transition(ProjectionState::Queued).is_err());
    }

    #[test]
    fn legacy_projected_state_migrates_to_canonical_live_state() {
        assert_eq!(
            serde_json::from_str::<ProjectionState>("\"Projected\"").unwrap(),
            ProjectionState::Live
        );
    }
}
