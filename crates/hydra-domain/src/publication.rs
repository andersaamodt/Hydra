use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublicationState {
    Draft,
    LocalSigned,
    Queued,
    Publishing,
    Published,
    Replicated,
    PartiallyFailed,
    Abandoned,
    MediaIncomplete,
    Failed,
    SourceUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicationStatus {
    pub state: PublicationState,
    pub accepted_relays: u16,
    pub attempted_relays: u16,
}

impl ReplicationStatus {
    #[must_use]
    pub fn from_counts(accepted_relays: u16, attempted_relays: u16, threshold: u16) -> Self {
        let state = if accepted_relays >= threshold && threshold > 1 {
            PublicationState::Replicated
        } else if accepted_relays > 0 {
            PublicationState::Published
        } else if attempted_relays > 0 {
            PublicationState::Failed
        } else {
            PublicationState::LocalSigned
        };
        Self {
            state,
            accepted_relays,
            attempted_relays,
        }
    }
}
