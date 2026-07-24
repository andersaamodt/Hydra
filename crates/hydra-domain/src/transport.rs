use serde::{Deserialize, Serialize};

use crate::DomainError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundEvent {
    pub event_id: String,
    pub event_json: String,
    pub relays: Vec<String>,
}

impl OutboundEvent {
    pub const MAX_EVENT_BYTES: usize = 256 * 1024;
    pub const MAX_RELAYS: usize = 32;

    /// Validates one queued network event without interpreting its protocol.
    ///
    /// # Errors
    ///
    /// Returns an error for unbounded JSON, identifiers, or relay destinations.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.event_id.trim().is_empty()
            || self.event_id.len() > 128
            || self.event_id.chars().any(char::is_whitespace)
            || self.event_json.is_empty()
            || self.event_json.len() > Self::MAX_EVENT_BYTES
            || self.relays.len() > Self::MAX_RELAYS
            || self.relays.iter().any(|relay| !valid_relay(relay))
            || self
                .relays
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.relays.len()
        {
            return Err(DomainError::InvalidObjectShape);
        }
        Ok(())
    }
}

pub(crate) fn valid_relay(relay: &str) -> bool {
    let authority = relay
        .strip_prefix("wss://")
        .or_else(|| relay.strip_prefix("ws://"));
    relay.len() <= 2_048
        && !relay.chars().any(char::is_whitespace)
        && authority.is_some_and(|authority| {
            !authority.is_empty()
                && !authority.starts_with('/')
                && authority
                    .split(['/', '?', '#'])
                    .next()
                    .is_some_and(|host| !host.is_empty())
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Accepted,
    Rejected { reason: String },
    Failed { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_validation_rejects_empty_and_path_only_authorities() {
        assert!(valid_relay("wss://relay.example"));
        assert!(valid_relay("ws://127.0.0.1:7777/path"));
        assert!(!valid_relay("wss://"));
        assert!(!valid_relay("wss:///local/path"));
        assert!(!valid_relay("https://relay.example"));
        assert!(!valid_relay("wss://relay.example\nforged"));
    }
}
