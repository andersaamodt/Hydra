use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use uuid::Uuid;

use crate::DomainError;

/// Local opaque identifier for one public Nostr persona.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PersonaId(Uuid);

impl PersonaId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parses a local persona identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the UUID representation is invalid.
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        Uuid::parse_str(value)
            .map(Self)
            .map_err(|_| DomainError::InvalidPersonaId)
    }
}

impl Default for PersonaId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for PersonaId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Adapter-owned Reddit identity value represented opaquely in the domain.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RedditAccountId(String);

impl RedditAccountId {
    pub const MAX_LEN: usize = 128;

    /// Wraps a non-empty adapter-owned Reddit account identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is empty.
    pub fn parse(value: impl Into<String>) -> Result<Self, crate::DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(crate::DomainError::Empty);
        }
        if value.len() > Self::MAX_LEN || value.chars().any(char::is_whitespace) {
            return Err(crate::DomainError::InvalidObjectShape);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NostrPublicKey(String);

impl NostrPublicKey {
    pub const MAX_LEN: usize = 128;

    /// Wraps a validated non-empty public-key representation.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty value. Cryptographic decoding belongs to the
    /// Nostr adapter and is not duplicated in the domain.
    pub fn parse(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DomainError::Empty);
        }
        if value.len() > Self::MAX_LEN || value.chars().any(char::is_whitespace) {
            return Err(DomainError::InvalidObjectShape);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NostrPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Persona {
    pub id: PersonaId,
    pub public_key: NostrPublicKey,
    pub display_name: String,
    pub reddit_account: Option<RedditAccountId>,
}

impl Persona {
    pub const MAX_DISPLAY_NAME_LEN: usize = 100;

    /// Validates a terminal public persona name without requiring a keypair.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, control-bearing, or
    /// directionally deceptive names.
    pub fn validate_display_name(value: &str) -> Result<(), DomainError> {
        if value.trim().is_empty()
            || value.len() > Self::MAX_DISPLAY_NAME_LEN
            || crate::text::has_unsafe_inline_text(value)
        {
            return Err(DomainError::InvalidObjectShape);
        }
        Ok(())
    }

    /// Revalidates public identity data after deserialization.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe public key, display name, or Reddit link.
    pub fn validate(&self) -> Result<(), DomainError> {
        NostrPublicKey::parse(self.public_key.as_str().to_owned())?;
        Self::validate_display_name(&self.display_name)?;
        if let Some(account) = &self.reddit_account {
            RedditAccountId::parse(account.as_str().to_owned())?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedditIdentityProof {
    pub persona: PersonaId,
    pub username: String,
    pub artifact_url: String,
    pub published_at: u64,
}

impl RedditIdentityProof {
    /// Validates the public NIP-39 Reddit proof convention without performing
    /// network I/O.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty identity or a non-Reddit HTTPS artifact.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.username.is_empty()
            || !self
                .username
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(DomainError::InvalidObjectShape);
        }
        if !self.artifact_url.starts_with("https://www.reddit.com/")
            || self.artifact_url.len() > 2_048
            || self.artifact_url.chars().any(char::is_whitespace)
        {
            return Err(DomainError::InvalidObjectShape);
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone)]
pub struct PersonaRegistry {
    personas: BTreeMap<PersonaId, Persona>,
    reddit_links: BTreeMap<RedditAccountId, PersonaId>,
}

impl PersonaRegistry {
    /// Adds a persona while enforcing the one-to-one Reddit mapping.
    ///
    /// # Errors
    ///
    /// Returns an error when a persona ID or Reddit account is already linked.
    pub fn insert(&mut self, persona: Persona) -> Result<(), DomainError> {
        persona.validate()?;
        if self.personas.contains_key(&persona.id) {
            return Err(DomainError::IdentityConflict);
        }
        if let Some(account) = &persona.reddit_account
            && self.reddit_links.contains_key(account)
        {
            return Err(DomainError::IdentityConflict);
        }
        if let Some(account) = &persona.reddit_account {
            self.reddit_links.insert(account.clone(), persona.id);
        }
        self.personas.insert(persona.id, persona);
        Ok(())
    }

    /// Replaces the mutable metadata for an existing persona while preserving
    /// the one-to-one Reddit-account invariant.
    ///
    /// # Errors
    ///
    /// Returns an error when the persona does not exist, its public key changes,
    /// or the requested Reddit account belongs to another persona.
    pub fn replace(&mut self, persona: Persona) -> Result<(), DomainError> {
        persona.validate()?;
        let current = self
            .personas
            .get(&persona.id)
            .ok_or(DomainError::MissingPersona)?;
        if current.public_key != persona.public_key {
            return Err(DomainError::IdentityConflict);
        }
        if let Some(owner) = persona
            .reddit_account
            .as_ref()
            .and_then(|account| self.reddit_links.get(account))
            && *owner != persona.id
        {
            return Err(DomainError::IdentityConflict);
        }
        if let Some(previous) = &current.reddit_account {
            self.reddit_links.remove(previous);
        }
        if let Some(account) = &persona.reddit_account {
            self.reddit_links.insert(account.clone(), persona.id);
        }
        self.personas.insert(persona.id, persona);
        Ok(())
    }

    #[must_use]
    pub fn get(&self, id: PersonaId) -> Option<&Persona> {
        self.personas.get(&id)
    }

    #[must_use]
    pub fn contains(&self, id: PersonaId) -> bool {
        self.personas.contains_key(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Persona> {
        self.personas.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persona(account: &str) -> Persona {
        Persona {
            id: PersonaId::new(),
            public_key: NostrPublicKey::parse(format!("npub-{account}")).unwrap(),
            display_name: account.to_owned(),
            reddit_account: Some(RedditAccountId::parse(account).unwrap()),
        }
    }

    #[test]
    fn reddit_link_is_strictly_one_to_one() {
        let mut registry = PersonaRegistry::default();
        registry.insert(persona("alice")).unwrap();
        assert_eq!(
            registry.insert(persona("alice")),
            Err(DomainError::IdentityConflict)
        );
    }

    #[test]
    fn persona_can_link_and_unlink_one_reddit_account() {
        let mut registry = PersonaRegistry::default();
        let mut item = Persona {
            id: PersonaId::new(),
            public_key: NostrPublicKey::parse("npub-alice").unwrap(),
            display_name: "Alice".to_owned(),
            reddit_account: None,
        };
        registry.insert(item.clone()).unwrap();

        item.reddit_account = Some(RedditAccountId::parse("alice").unwrap());
        registry.replace(item.clone()).unwrap();
        assert_eq!(
            registry
                .get(item.id)
                .unwrap()
                .reddit_account
                .as_ref()
                .unwrap()
                .as_str(),
            "alice"
        );

        item.reddit_account = None;
        registry.replace(item.clone()).unwrap();
        assert!(registry.get(item.id).unwrap().reddit_account.is_none());
    }
}
