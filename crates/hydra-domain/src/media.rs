use serde::{Deserialize, Serialize};

use crate::{AnchorId, DomainError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaManifest {
    pub object: AnchorId,
    pub sha256: String,
    pub mime_type: String,
    pub size: u64,
    #[serde(default)]
    pub dimensions: Option<String>,
    #[serde(default)]
    pub duration_seconds: Option<u64>,
    pub local_path: String,
    pub original_url: Option<String>,
    #[serde(default)]
    pub blob_urls: Vec<String>,
    #[serde(default)]
    pub metadata_event_id: Option<String>,
    pub preserved_at: u64,
}

impl MediaManifest {
    pub const MAX_BYTES: u64 = 100 * 1024 * 1024;

    /// Validates a bounded content-addressed media record.
    ///
    /// # Errors
    ///
    /// Returns an error for missing metadata, invalid hashes, or oversized data.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.mime_type.trim().is_empty() || self.local_path.trim().is_empty() {
            return Err(DomainError::Empty);
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(DomainError::InvalidManifest);
        }
        if self.size > Self::MAX_BYTES {
            return Err(DomainError::TooLong {
                max: usize::try_from(Self::MAX_BYTES).unwrap_or(usize::MAX),
            });
        }
        if self
            .dimensions
            .as_deref()
            .is_some_and(|value| !valid_dimensions(value))
            || self
                .blob_urls
                .iter()
                .any(|value| !valid_public_blob_url(value))
            || self.metadata_event_id.as_deref().is_some_and(|value| {
                value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err(DomainError::InvalidManifest);
        }
        Ok(())
    }
}

fn valid_dimensions(value: &str) -> bool {
    let Some((width, height)) = value.split_once('x') else {
        return false;
    };
    !width.is_empty()
        && !height.is_empty()
        && width.bytes().all(|byte| byte.is_ascii_digit())
        && height.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_public_blob_url(value: &str) -> bool {
    value.starts_with("https://") && value.len() <= 2_048 && !value.chars().any(char::is_whitespace)
}
