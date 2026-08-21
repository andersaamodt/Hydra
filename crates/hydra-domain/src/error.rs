use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("value cannot be empty")]
    Empty,
    #[error("value exceeds the maximum length of {max}")]
    TooLong { max: usize },
    #[error("invalid community key")]
    InvalidCommunityKey,
    #[error("invalid state transition from {from} to {to}")]
    InvalidTransition { from: String, to: String },
    #[error("content manifest is internally inconsistent")]
    InvalidManifest,
    #[error("identity is already registered or linked")]
    IdentityConflict,
    #[error("delivery references an unknown outbound event")]
    UnknownOutboundEvent,
    #[error("persona identifier is invalid")]
    InvalidPersonaId,
    #[error("persona does not exist")]
    MissingPersona,
    #[error("reaction is invalid")]
    InvalidReaction,
    #[error("revisit intent and schedule are inconsistent")]
    InvalidRevisit,
    #[error("discussion object does not exist")]
    MissingObject,
    #[error("object shape is invalid for its kind")]
    InvalidObjectShape,
    #[error("operation identifier is invalid")]
    InvalidOperationId,
    #[error("projection identifier is invalid")]
    InvalidProjectionId,
    #[error("Flocking state is invalid")]
    InvalidFlocking,
}
