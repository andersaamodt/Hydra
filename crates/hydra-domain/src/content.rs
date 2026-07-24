use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

use crate::{CommunityKey, DomainError, NostrPublicKey, PersonaId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArchiveId(Uuid);

impl ArchiveId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ArchiveId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ArchiveId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreservationLevel {
    Item,
    Ancestors,
    VisibleSiblings,
    LoadedThread,
}

/// Exact capture receipt. `loaded` and `preserved` are separate so a Level 3
/// archive never implies knowledge of Reddit objects the client did not load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveManifest {
    pub id: ArchiveId,
    pub observer: PersonaId,
    pub selected: ExternalId,
    pub level: PreservationLevel,
    pub loaded: Vec<ExternalId>,
    pub preserved: Vec<ExternalId>,
    #[serde(default)]
    pub media_preserved: Vec<ExternalId>,
    #[serde(default)]
    pub media_unavailable: Vec<ExternalId>,
    pub captured_at: u64,
}

impl ArchiveManifest {
    pub const MAX_OBJECTS: usize = 5_000;

    /// Validates that the receipt names exactly at least one preserved object
    /// and that the selected object is among them.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty or internally inconsistent manifest.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.loaded.is_empty()
            || self.preserved.is_empty()
            || !self.preserved.contains(&self.selected)
            || self.loaded.len() > Self::MAX_OBJECTS
            || self.preserved.len() > Self::MAX_OBJECTS
            || self.media_preserved.len() > Self::MAX_OBJECTS
            || self.media_unavailable.len() > Self::MAX_OBJECTS
            || self
                .loaded
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.loaded.len()
            || self
                .preserved
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.preserved.len()
            || self
                .media_preserved
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.media_preserved.len()
            || self
                .media_unavailable
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.media_unavailable.len()
            || self
                .preserved
                .iter()
                .any(|item| !self.loaded.contains(item))
            || self
                .media_preserved
                .iter()
                .any(|item| self.media_unavailable.contains(item))
        {
            return Err(DomainError::InvalidManifest);
        }
        self.selected.validate()?;
        for identifier in self
            .loaded
            .iter()
            .chain(&self.preserved)
            .chain(&self.media_preserved)
            .chain(&self.media_unavailable)
        {
            identifier.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AnchorId(String);

impl AnchorId {
    pub const MAX_LEN: usize = 256;

    /// Creates a non-empty stable anchor identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is empty.
    pub fn parse(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DomainError::Empty);
        }
        if value.len() > Self::MAX_LEN {
            return Err(DomainError::TooLong { max: Self::MAX_LEN });
        }
        if value.chars().any(char::is_control) {
            return Err(DomainError::InvalidObjectShape);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentBody(String);

impl ContentBody {
    pub const MAX_LEN: usize = 100_000;

    /// Creates a bounded non-empty content body.
    ///
    /// # Errors
    ///
    /// Returns an error when the body is empty or exceeds [`Self::MAX_LEN`].
    pub fn parse(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DomainError::Empty);
        }
        if value.len() > Self::MAX_LEN {
            return Err(DomainError::TooLong { max: Self::MAX_LEN });
        }
        Ok(Self(value))
    }

    /// Creates a bounded post body, including the empty body used by link
    /// posts. Comments and norms continue to use [`Self::parse`].
    ///
    /// # Errors
    ///
    /// Returns an error when the body exceeds [`Self::MAX_LEN`].
    pub fn parse_post(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.len() > Self::MAX_LEN {
            return Err(DomainError::TooLong { max: Self::MAX_LEN });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Post,
    Comment,
    Norm,
}

/// Latest editable representation of an immutable Nostr anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectHead {
    pub anchor: AnchorId,
    pub author: NostrPublicKey,
    pub kind: ObjectKind,
    pub title: Option<String>,
    pub body: ContentBody,
    pub communities: Vec<CommunityKey>,
    pub root: Option<AnchorId>,
    pub parent: Option<AnchorId>,
    #[serde(default)]
    pub external_root: Option<ExternalId>,
    #[serde(default)]
    pub external_parent: Option<ExternalId>,
    #[serde(default)]
    pub external_source: Option<ExternalId>,
    pub edited_at: u64,
}

impl ObjectHead {
    pub const MAX_TITLE_LEN: usize = 300;
    pub const MAX_COMMUNITIES: usize = 32;

    /// Validates a short, public Hydra-authored title.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty, oversized, control-bearing, or
    /// directionally deceptive title.
    pub fn validate_title(value: &str) -> Result<(), DomainError> {
        if value.trim().is_empty()
            || value.len() > Self::MAX_TITLE_LEN
            || crate::text::has_unsafe_inline_text(value)
        {
            return Err(DomainError::InvalidObjectShape);
        }
        Ok(())
    }

    /// Validates a materialized discussion object, including values that came
    /// from deserialization rather than Hydra's ordinary constructors.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized metadata, duplicate/empty communities,
    /// or a title shape that does not match the object kind.
    pub fn validate(&self) -> Result<(), DomainError> {
        AnchorId::parse(self.anchor.as_str())?;
        match self.kind {
            ObjectKind::Post => {
                ContentBody::parse_post(self.body.as_str().to_owned())?;
            }
            ObjectKind::Comment | ObjectKind::Norm => {
                ContentBody::parse(self.body.as_str().to_owned())?;
            }
        }
        if self.communities.len() > Self::MAX_COMMUNITIES
            || self
                .communities
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.communities.len()
        {
            return Err(DomainError::InvalidObjectShape);
        }
        for identifier in [
            &self.external_root,
            &self.external_parent,
            &self.external_source,
        ]
        .into_iter()
        .flatten()
        {
            identifier.validate()?;
        }
        match (&self.kind, &self.title) {
            (ObjectKind::Comment | ObjectKind::Post, None) => {}
            (ObjectKind::Post | ObjectKind::Norm, Some(title))
                if Self::validate_title(title).is_ok() => {}
            _ => return Err(DomainError::InvalidObjectShape),
        }
        Ok(())
    }

    #[must_use]
    pub fn revised(&self, body: ContentBody, edited_at: u64) -> Self {
        Self {
            anchor: self.anchor.clone(),
            author: self.author.clone(),
            kind: self.kind,
            title: self.title.clone(),
            body,
            communities: self.communities.clone(),
            root: self.root.clone(),
            parent: self.parent.clone(),
            external_root: self.external_root.clone(),
            external_parent: self.external_parent.clone(),
            external_source: self.external_source.clone(),
            edited_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExternalId {
    pub system: String,
    pub canonical: String,
}

impl ExternalId {
    pub const MAX_SYSTEM_LEN: usize = 32;
    pub const MAX_CANONICAL_LEN: usize = 4_096;

    /// Creates an external identifier without interpreting adapter-owned values.
    ///
    /// # Errors
    ///
    /// Returns an error when the system or canonical identifier is empty.
    pub fn new(
        system: impl Into<String>,
        canonical: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let system = system.into();
        let canonical = canonical.into();
        if system.trim().is_empty() || canonical.trim().is_empty() {
            return Err(DomainError::Empty);
        }
        if system.len() > Self::MAX_SYSTEM_LEN
            || canonical.len() > Self::MAX_CANONICAL_LEN
            || !system
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || canonical.chars().any(char::is_control)
        {
            return Err(DomainError::InvalidObjectShape);
        }
        Ok(Self { system, canonical })
    }

    /// Revalidates an identifier received through deserialization.
    ///
    /// # Errors
    ///
    /// Returns an error when either component is unsafe or unbounded.
    pub fn validate(&self) -> Result<(), DomainError> {
        Self::new(self.system.clone(), self.canonical.clone()).map(|_| ())
    }
}

#[cfg(test)]
mod content_tests {
    use crate::PersonaId;

    use super::{
        ArchiveId, ArchiveManifest, ContentBody, ExternalId, ObjectHead, PreservationLevel,
    };

    #[test]
    fn link_posts_may_be_empty_without_weakening_comment_bodies() {
        assert_eq!(ContentBody::parse_post("").unwrap().as_str(), "");
        assert!(ContentBody::parse("").is_err());
    }

    #[test]
    fn native_titles_reject_directional_spoofing_but_accept_rtl() {
        assert!(ObjectHead::validate_title("Hydra \u{202e}ardyH").is_err());
        assert!(ObjectHead::validate_title("هايدرا").is_ok());
    }

    #[test]
    fn archive_receipts_reject_duplicate_or_malformed_media_claims() {
        let selected = ExternalId::new("reddit-fullname", "t3_abc").unwrap();
        let media = ExternalId::new("url", "https://example.org/a").unwrap();
        let mut manifest = ArchiveManifest {
            id: ArchiveId::new(),
            observer: PersonaId::new(),
            selected: selected.clone(),
            level: PreservationLevel::Item,
            loaded: vec![selected.clone()],
            preserved: vec![selected],
            media_preserved: vec![media.clone()],
            media_unavailable: Vec::new(),
            captured_at: 1,
        };
        assert!(manifest.validate().is_ok());
        manifest.media_preserved.push(media);
        assert!(manifest.validate().is_err());
    }
}
