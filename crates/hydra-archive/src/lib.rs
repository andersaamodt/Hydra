#![forbid(unsafe_code)]
//! Interaction-driven capture scopes.

use std::collections::BTreeSet;

use hydra_domain::{ArchiveManifest, DomainError, PreservationLevel};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureScope {
    pub level: PreservationLevel,
    pub objects: Vec<String>,
    pub claims_complete: bool,
}

impl CaptureScope {
    #[must_use]
    pub fn selected(
        level: PreservationLevel,
        subject: impl Into<String>,
        ancestors: &[String],
        visible_siblings: &[String],
        loaded_thread: &[String],
    ) -> Self {
        let subject = subject.into();
        let mut objects = vec![subject];
        match level {
            PreservationLevel::Item => {}
            PreservationLevel::Ancestors => objects.extend_from_slice(ancestors),
            PreservationLevel::VisibleSiblings => {
                objects.extend_from_slice(ancestors);
                objects.extend_from_slice(visible_siblings);
            }
            PreservationLevel::LoadedThread => objects.extend_from_slice(loaded_thread),
        }
        let mut seen = BTreeSet::new();
        objects.retain(|object| seen.insert(object.clone()));
        Self {
            level,
            objects,
            // Level 3 means the entire loaded view, never unseen completeness.
            claims_complete: false,
        }
    }
}

/// Validates that an archive receipt remains an exact statement of captured
/// material rather than a claim about unseen source completeness.
///
/// # Errors
///
/// Returns an error for an invalid manifest or a Level 3 receipt with no
/// recorded visible object.
pub fn validate_manifest(manifest: &ArchiveManifest) -> Result<(), DomainError> {
    manifest.validate()?;
    if manifest.level == PreservationLevel::LoadedThread && manifest.loaded.is_empty() {
        return Err(DomainError::InvalidObjectShape);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_three_names_only_loaded_objects_and_never_claims_completeness() {
        let scope = CaptureScope::selected(
            PreservationLevel::LoadedThread,
            "subject",
            &[],
            &[],
            &["subject".to_owned(), "loaded-reply".to_owned()],
        );
        assert_eq!(scope.objects, ["subject", "loaded-reply"]);
        assert!(!scope.claims_complete);
    }
}
