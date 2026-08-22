use serde::{Deserialize, Serialize};

use crate::{AnchorId, CommunityKey, DomainError, NostrPublicKey};

/// A short public label displayed beside a persona or post.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FlairText(String);

impl FlairText {
    pub const MAX_CHARS: usize = 32;

    /// Parses a visible flair label.
    ///
    /// # Errors
    ///
    /// Returns an error for blank, oversized, control, or bidi-control text.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, DomainError> {
        let value = value.as_ref().trim();
        if value.is_empty() {
            return Err(DomainError::Empty);
        }
        if value.chars().count() > Self::MAX_CHARS {
            return Err(DomainError::TooLong {
                max: Self::MAX_CHARS,
            });
        }
        if crate::text::has_unsafe_inline_text(value) {
            return Err(DomainError::InvalidObjectShape);
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaProfile {
    pub public_key: NostrPublicKey,
    pub display_name: String,
    pub flair: Option<FlairText>,
    pub source_event_id: String,
    pub updated_at: u64,
}

impl PersonaProfile {
    /// # Errors
    ///
    /// Returns an error when any public profile field is malformed.
    pub fn validate(&self) -> Result<(), DomainError> {
        crate::Persona::validate_display_name(&self.display_name)?;
        if let Some(flair) = &self.flair {
            FlairText::parse(flair.as_str())?;
        }
        validate_source_event_id(&self.source_event_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostFlairScope {
    All,
    Community(CommunityKey),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostFlairChoice {
    pub author: NostrPublicKey,
    pub target: AnchorId,
    pub scope: PostFlairScope,
    /// `None` is an explicit withdrawal of this scope's choice.
    pub flair: Option<FlairText>,
    pub changed_at: u64,
    pub source_event_id: String,
}

impl PostFlairChoice {
    /// # Errors
    ///
    /// Returns an error when the target, flair, or source identifier is malformed.
    pub fn validate(&self) -> Result<(), DomainError> {
        AnchorId::parse(self.target.as_str())?;
        if let PostFlairScope::Community(community) = &self.scope {
            CommunityKey::parse(community.as_str())?;
        }
        if let Some(flair) = &self.flair {
            FlairText::parse(flair.as_str())?;
        }
        validate_source_event_id(&self.source_event_id)
    }
}

fn validate_source_event_id(value: &str) -> Result<(), DomainError> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_whitespace) {
        return Err(DomainError::InvalidObjectShape);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flair_is_trimmed_and_bounded_by_visible_characters() {
        assert_eq!(
            FlairText::parse("  Question  ").unwrap().as_str(),
            "Question"
        );
        assert!(FlairText::parse("🦀".repeat(32)).is_ok());
        assert!(FlairText::parse("🦀".repeat(33)).is_err());
        assert!(FlairText::parse("spoof\u{202e}").is_err());
    }
}
