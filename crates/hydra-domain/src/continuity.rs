//! Authoritative continuity-system state machines.

use serde::{Deserialize, Serialize};

use crate::{DomainError, OperationId, PersonaId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BigStickState {
    Requested,
    IdentifyingObject,
    Archiving,
    Verifying,
    EditingReddit,
    Complete,
    ArchiveFailed,
    EditFailed,
    Canceled,
}

impl BigStickState {
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        use BigStickState::{
            ArchiveFailed, Archiving, Canceled, Complete, EditFailed, EditingReddit,
            IdentifyingObject, Requested, Verifying,
        };
        matches!(
            (self, next),
            (Requested, IdentifyingObject | Canceled)
                | (IdentifyingObject, Archiving | ArchiveFailed | Canceled)
                | (Archiving, Verifying | ArchiveFailed | Canceled)
                | (Verifying, EditingReddit | ArchiveFailed | Canceled)
                | (EditingReddit, Complete | EditFailed)
                | (EditFailed, EditingReddit | Canceled)
        ) || self as u8 == next as u8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReddactedState {
    Requested,
    Previewed,
    Archiving,
    Verified,
    Withdrawing,
    Withdrawn,
    PartiallyFailed,
    Failed,
    Canceled,
}

impl ReddactedState {
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        use ReddactedState::{
            Archiving, Canceled, Failed, PartiallyFailed, Previewed, Requested, Verified,
            Withdrawing, Withdrawn,
        };
        matches!(
            (self, next),
            (Requested, Previewed | Canceled)
                | (Previewed, Archiving | Canceled)
                | (Archiving, Verified | Failed | Canceled)
                | (Verified, Withdrawing | Canceled)
                | (Withdrawing, Withdrawn | PartiallyFailed | Failed)
                | (PartiallyFailed | Failed, Archiving | Withdrawing | Canceled)
        ) || self as u8 == next as u8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PersonaSwitchState {
    Active,
    Switching,
    Locked,
    Unavailable,
}

/// One durably journaled continuity workflow. The variants remain distinct so
/// a generic operation status cannot erase the safety order of the canonical
/// Big Stick, Reddacted, or persona-switch state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "workflow", content = "state", rename_all = "snake_case")]
pub enum ContinuityState {
    BigStick(BigStickState),
    Reddacted(ReddactedState),
    PersonaSwitch(PersonaSwitchState),
}

impl ContinuityState {
    #[must_use]
    pub const fn is_initial(self) -> bool {
        matches!(
            self,
            Self::BigStick(BigStickState::Requested)
                | Self::Reddacted(ReddactedState::Requested)
                | Self::PersonaSwitch(PersonaSwitchState::Active)
        )
    }

    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        match (self, next) {
            (Self::BigStick(current), Self::BigStick(next)) => current.can_transition_to(next),
            (Self::Reddacted(current), Self::Reddacted(next)) => current.can_transition_to(next),
            (Self::PersonaSwitch(current), Self::PersonaSwitch(next)) => {
                current.can_transition_to(next)
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuityWorkflow {
    pub id: OperationId,
    pub persona: PersonaId,
    #[serde(default)]
    pub subject: Option<String>,
    pub state: ContinuityState,
}

impl ContinuityWorkflow {
    /// Applies one transition without permitting a workflow to change type.
    ///
    /// # Errors
    ///
    /// Returns an error when the canonical state machine rejects the step.
    pub fn transition(&mut self, next: ContinuityState) -> Result<(), DomainError> {
        if self.state.can_transition_to(next) {
            self.state = next;
            Ok(())
        } else {
            Err(DomainError::InvalidTransition {
                from: format!("{:?}", self.state),
                to: format!("{next:?}"),
            })
        }
    }
}

impl PersonaSwitchState {
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        use PersonaSwitchState::{Active, Locked, Switching, Unavailable};
        matches!(
            (self, next),
            (Active, Switching | Locked)
                | (Switching, Active | Locked | Unavailable)
                | (Locked | Unavailable, Switching)
        ) || self as u8 == next as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preservation_must_precede_external_mutation() {
        assert!(!BigStickState::Archiving.can_transition_to(BigStickState::EditingReddit));
        assert!(BigStickState::Verifying.can_transition_to(BigStickState::EditingReddit));
        assert!(!ReddactedState::Archiving.can_transition_to(ReddactedState::Withdrawing));
        assert!(ReddactedState::Verified.can_transition_to(ReddactedState::Withdrawing));
    }
}
