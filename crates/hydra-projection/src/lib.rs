#![forbid(unsafe_code)]
//! Platform-neutral external projection policy.

use hydra_domain::{Projection, ProjectionId, ProjectionState};

#[derive(Debug, Clone, Copy)]
pub struct WithdrawalSelection<'a> {
    pub selected: Option<&'a std::collections::BTreeSet<ProjectionId>>,
}

impl WithdrawalSelection<'_> {
    #[must_use]
    pub fn includes(&self, projection: &Projection) -> bool {
        projection.state != ProjectionState::Withdrawn
            && self
                .selected
                .is_none_or(|selected| selected.contains(&projection.id))
    }
}

#[must_use]
pub const fn state_is_live(state: ProjectionState) -> bool {
    matches!(
        state,
        ProjectionState::Live | ProjectionState::Synchronizing | ProjectionState::Diverged
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn withdrawn_is_never_live() {
        assert!(state_is_live(ProjectionState::Live));
        assert!(!state_is_live(ProjectionState::Withdrawn));
    }
}
