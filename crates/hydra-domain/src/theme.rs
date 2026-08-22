use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{CommunityKey, DomainError, NostrPublicKey};

pub const COMMUNITY_COLOR_SCHEME_VERSION: &str = "1";

/// Four human-selected identity colors. Hydra derives all component colors and text contrast.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CommunityColorScheme {
    pub light_base: String,
    pub light_accent: String,
    pub dark_base: String,
    pub dark_accent: String,
}

impl CommunityColorScheme {
    /// Accepts only canonical, opaque six-digit sRGB colors.
    ///
    /// # Errors
    ///
    /// Returns an error when any color is not lowercase `#rrggbb`.
    pub fn validate(&self) -> Result<(), DomainError> {
        if [
            &self.light_base,
            &self.light_accent,
            &self.dark_base,
            &self.dark_accent,
        ]
        .into_iter()
        .all(|color| {
            color.len() == 7
                && color.starts_with('#')
                && color[1..]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            Ok(())
        } else {
            Err(DomainError::InvalidObjectShape)
        }
    }
}

/// One replaceable color-scheme choice for one ownerless topic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommunityColorChoice {
    pub author: NostrPublicKey,
    pub topic: CommunityKey,
    pub scheme: Option<CommunityColorScheme>,
    pub created_at: u64,
    pub event_id: Option<String>,
}

impl CommunityColorChoice {
    /// Revalidates the bounded palette and optional source-event identity.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed colors or event IDs.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.event_id.as_ref().is_some_and(|event_id| {
            event_id.len() != 64 || !event_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(DomainError::InvalidObjectShape);
        }
        if let Some(scheme) = &self.scheme {
            scheme.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub struct CommunityColorInput<'a> {
    pub persona: &'a NostrPublicKey,
    pub topic: &'a CommunityKey,
    pub selected_sources: &'a BTreeSet<NostrPublicKey>,
    pub complete_sources: &'a BTreeSet<NostrPublicKey>,
    pub choices: &'a [CommunityColorChoice],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunityColorResult {
    pub scheme: Option<CommunityColorScheme>,
    pub direct: bool,
    pub sources: Vec<NostrPublicKey>,
    pub incomplete_sources: Vec<NostrPublicKey>,
}

/// Resolves a direct choice first, then exact palette support among explicitly selected sources.
#[must_use]
pub fn evaluate_community_colors(input: CommunityColorInput<'_>) -> CommunityColorResult {
    let mut current: BTreeMap<&NostrPublicKey, &CommunityColorChoice> = BTreeMap::new();
    for choice in input
        .choices
        .iter()
        .filter(|choice| &choice.topic == input.topic && choice.validate().is_ok())
    {
        let replace = current.get(&choice.author).is_none_or(|prior| {
            choice.created_at > prior.created_at
                || (choice.created_at == prior.created_at && choice.event_id < prior.event_id)
        });
        if replace {
            current.insert(&choice.author, choice);
        }
    }
    if let Some(choice) = current.get(input.persona)
        && choice.scheme.is_some()
    {
        return CommunityColorResult {
            scheme: choice.scheme.clone(),
            direct: true,
            sources: vec![input.persona.clone()],
            incomplete_sources: Vec::new(),
        };
    }

    let mut support: BTreeMap<CommunityColorScheme, (BTreeSet<NostrPublicKey>, u64)> =
        BTreeMap::new();
    for source in input.selected_sources {
        let Some(choice) = current.get(source) else {
            continue;
        };
        let Some(scheme) = &choice.scheme else {
            continue;
        };
        let entry = support
            .entry(scheme.clone())
            .or_insert_with(|| (BTreeSet::new(), choice.created_at));
        entry.0.insert(source.clone());
        entry.1 = entry.1.max(choice.created_at);
    }
    let selected = support.into_iter().max_by(|left, right| {
        left.1
            .0
            .len()
            .cmp(&right.1.0.len())
            .then_with(|| left.1.1.cmp(&right.1.1))
            .then_with(|| right.0.cmp(&left.0))
    });
    CommunityColorResult {
        scheme: selected.as_ref().map(|(scheme, _)| scheme.clone()),
        direct: false,
        sources: selected.map_or_else(Vec::new, |(_, (sources, _))| sources.into_iter().collect()),
        incomplete_sources: input
            .selected_sources
            .difference(input.complete_sources)
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: char) -> NostrPublicKey {
        NostrPublicKey::parse(value.to_string().repeat(64)).unwrap()
    }

    fn scheme(value: char) -> CommunityColorScheme {
        let color = format!("#{value}{value}{value}{value}{value}{value}");
        CommunityColorScheme {
            light_base: color.clone(),
            light_accent: color.clone(),
            dark_base: color.clone(),
            dark_accent: color,
        }
    }

    fn choice(
        author: char,
        colors: Option<CommunityColorScheme>,
        created_at: u64,
    ) -> CommunityColorChoice {
        CommunityColorChoice {
            author: key(author),
            topic: CommunityKey::parse("science").unwrap(),
            scheme: colors,
            created_at,
            event_id: None,
        }
    }

    #[test]
    fn direct_choice_precedes_followed_support() {
        let persona = key('a');
        let source = key('b');
        let topic = CommunityKey::parse("science").unwrap();
        let choices = vec![
            choice('a', Some(scheme('1')), 1),
            choice('b', Some(scheme('2')), 2),
        ];
        let selected_sources = BTreeSet::from([source.clone()]);
        let result = evaluate_community_colors(CommunityColorInput {
            persona: &persona,
            topic: &topic,
            selected_sources: &selected_sources,
            complete_sources: &selected_sources,
            choices: &choices,
        });
        assert_eq!(result.scheme, Some(scheme('1')));
        assert!(result.direct);
        assert_eq!(result.sources, vec![persona]);
    }

    #[test]
    fn followed_choices_converge_by_exact_palette() {
        let persona = key('a');
        let topic = CommunityKey::parse("science").unwrap();
        let selected_sources = BTreeSet::from([key('b'), key('c'), key('d')]);
        let choices = vec![
            choice('b', Some(scheme('1')), 1),
            choice('c', Some(scheme('1')), 2),
            choice('d', Some(scheme('2')), 3),
        ];
        let result = evaluate_community_colors(CommunityColorInput {
            persona: &persona,
            topic: &topic,
            selected_sources: &selected_sources,
            complete_sources: &selected_sources,
            choices: &choices,
        });
        assert_eq!(result.scheme, Some(scheme('1')));
        assert_eq!(result.sources, vec![key('b'), key('c')]);
        assert!(!result.direct);
    }
}
