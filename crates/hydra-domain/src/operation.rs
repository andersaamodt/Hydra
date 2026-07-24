use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

use crate::DomainError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationId(Uuid);

impl OperationId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parses a durable operation identifier.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid UUID.
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        Uuid::parse_str(value)
            .map(Self)
            .map_err(|_| DomainError::InvalidOperationId)
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Default for OperationId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl OperationState {
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Queued, Self::Running | Self::Cancelled)
                | (
                    Self::Running,
                    Self::Succeeded | Self::Failed | Self::Cancelled
                )
                | (Self::Failed, Self::Queued)
        ) || self as u8 == next as u8
    }

    /// Applies a legal operation transition.
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal state would be restarted or a lifecycle
    /// phase would be skipped.
    pub fn transition(self, next: Self) -> Result<Self, DomainError> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(DomainError::InvalidTransition {
                from: format!("{self:?}"),
                to: format!("{next:?}"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_can_retry_but_success_is_terminal() {
        assert_eq!(
            OperationState::Failed.transition(OperationState::Queued),
            Ok(OperationState::Queued)
        );
        assert!(
            OperationState::Succeeded
                .transition(OperationState::Queued)
                .is_err()
        );
    }
}
