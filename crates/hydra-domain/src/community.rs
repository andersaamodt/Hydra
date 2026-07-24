use serde::{Deserialize, Serialize};

use crate::DomainError;

/// A normalized ownerless Hydra community topic, without `/h/`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommunityKey(String);

impl CommunityKey {
    pub const MAX_LEN: usize = 64;

    /// Parses and normalizes a bare Hydra community topic.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is empty, too long, or contains characters
    /// outside lowercase ASCII letters, digits, and underscores.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, DomainError> {
        let normalized = value.as_ref().trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(DomainError::Empty);
        }
        if normalized.len() > Self::MAX_LEN {
            return Err(DomainError::TooLong { max: Self::MAX_LEN });
        }
        if !normalized
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(DomainError::InvalidCommunityKey);
        }
        Ok(Self(normalized))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn hydra_path(&self) -> String {
        format!("/h/{}", self.0)
    }

    #[must_use]
    pub fn corresponding_reddit_path(&self) -> String {
        format!("/r/{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_a_key_without_collapsing_namespaces() {
        let key = CommunityKey::parse(" Science ").unwrap();
        assert_eq!(key.as_str(), "science");
        assert_eq!(key.hydra_path(), "/h/science");
        assert_eq!(key.corresponding_reddit_path(), "/r/science");
    }

    #[test]
    fn rejects_paths_as_keys() {
        assert_eq!(
            CommunityKey::parse("/h/science"),
            Err(DomainError::InvalidCommunityKey)
        );
    }
}
