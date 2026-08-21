#![forbid(unsafe_code)]
//! Transparent feed, ranking, filtering, and Revisit lenses.

use std::{cmp::Reverse, collections::BTreeSet};

use hydra_domain::{
    AnchorId, CommunityKey, NostrPublicKey, ObjectHead, ObjectKind, PersonaId, PrivateState,
    ReactionValue,
};
use hydra_store::{DurableStore, Settings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedLens {
    New,
    Old,
    Top,
    Following,
    Trusted,
    Discussed,
    Controversial,
    Revisited,
    Recovered,
}

impl FeedLens {
    pub const ALL: [Self; 9] = [
        Self::New,
        Self::Old,
        Self::Top,
        Self::Following,
        Self::Trusted,
        Self::Discussed,
        Self::Controversial,
        Self::Revisited,
        Self::Recovered,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Old => "old",
            Self::Top => "top",
            Self::Following => "following",
            Self::Trusted => "trusted",
            Self::Discussed => "discussed",
            Self::Controversial => "controversial",
            Self::Revisited => "revisited",
            Self::Recovered => "recovered",
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FeedService;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgmentExclusion {
    Block,
    Silence,
    Hide,
    CommunityRemoval,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the decision exposes independent policy and evidence facts to the UI"
)]
pub struct VisibilityDecision {
    pub excluded: bool,
    pub uncertain: bool,
    pub exclusion: Option<JudgmentExclusion>,
    pub inherited: bool,
    pub source: Option<String>,
    pub event_id: Option<String>,
    pub reason: Option<String>,
    pub scope: Option<String>,
    pub why: Option<String>,
    pub cutoff: Option<u64>,
    pub local_timing_evidence: bool,
}

pub type BlockDecision = VisibilityDecision;

impl VisibilityDecision {
    fn allowed() -> Self {
        Self {
            excluded: false,
            uncertain: false,
            exclusion: None,
            inherited: false,
            source: None,
            event_id: None,
            reason: None,
            scope: None,
            why: None,
            cutoff: None,
            local_timing_evidence: false,
        }
    }
}

impl FeedService {
    #[must_use]
    pub fn public_feed(store: &DurableStore, lens: FeedLens) -> Vec<ObjectHead> {
        let mut heads = store
            .state()
            .heads
            .current_heads()
            .filter(|head| head.kind != ObjectKind::Comment)
            .filter(|_| {
                !matches!(
                    lens,
                    FeedLens::Following
                        | FeedLens::Trusted
                        | FeedLens::Revisited
                        | FeedLens::Recovered
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        sort_feed(store, &mut heads, lens);
        heads
    }

    #[must_use]
    pub fn feed(
        store: &DurableStore,
        private: &PrivateState,
        persona: PersonaId,
        settings: &Settings,
        lens: FeedLens,
    ) -> Vec<ObjectHead> {
        let followed = followed_authors(store, private, persona);
        let revisited = private.revisits.keys().cloned().collect::<BTreeSet<_>>();
        let mut heads = store
            .state()
            .heads
            .current_heads()
            .filter(|head| head.kind != ObjectKind::Comment)
            .filter(|head| passes_non_block_filters(store, private, settings, head))
            .filter(|head| match lens {
                FeedLens::Following | FeedLens::Trusted => followed.contains(&head.author),
                FeedLens::Revisited => revisited.contains(&head.anchor),
                FeedLens::Recovered => false,
                _ => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        sort_feed(store, &mut heads, lens);
        heads
    }

    #[must_use]
    pub fn community(
        store: &DurableStore,
        private: &PrivateState,
        persona: PersonaId,
        settings: &Settings,
        community: &CommunityKey,
        lens: FeedLens,
    ) -> Vec<ObjectHead> {
        Self::feed(store, private, persona, settings, lens)
            .into_iter()
            .filter(|head| head.communities.contains(community))
            .collect()
    }

    #[must_use]
    pub fn my_feed(
        store: &DurableStore,
        private: &PrivateState,
        persona: PersonaId,
        settings: &Settings,
    ) -> Vec<ObjectHead> {
        let followed = followed_authors(store, private, persona);
        let subscribed = subscribed_topics(store, private, persona);
        let revisited = private.revisits.keys().cloned().collect::<BTreeSet<_>>();
        let mut heads = store
            .state()
            .heads
            .current_heads()
            .filter(|head| head.kind != ObjectKind::Comment)
            .filter(|head| passes_non_block_filters(store, private, settings, head))
            .filter_map(|head| {
                let weights = &settings.feed_source_weights;
                let mut weight =
                    (&head.author == persona_public_key(store, persona)?).then_some(100);
                if followed.contains(&head.author) {
                    weight = weight.max(weights.get("followed").copied());
                }
                if head
                    .communities
                    .iter()
                    .any(|community| subscribed.contains(community))
                {
                    weight = weight.max(weights.get("communities").copied());
                }
                if thread_involves_persona(store, &head.anchor, persona_public_key(store, persona)?)
                {
                    weight = weight.max(weights.get("replies").copied());
                }
                if revisited.contains(&head.anchor) {
                    weight = weight.max(weights.get("revisit").copied());
                }
                weight.map(|weight| (head.clone(), weight))
            })
            .collect::<Vec<_>>();
        heads.sort_by_key(|(head, weight)| {
            (
                Reverse(*weight),
                Reverse(head.edited_at),
                head.anchor.clone(),
            )
        });
        heads.into_iter().map(|(head, _)| head).collect()
    }

    #[must_use]
    pub fn visible(
        store: &DurableStore,
        private: &PrivateState,
        persona: PersonaId,
        settings: &Settings,
        head: &ObjectHead,
    ) -> bool {
        Self::visible_in(store, private, persona, settings, head, None)
    }

    #[must_use]
    pub fn visible_in(
        store: &DurableStore,
        private: &PrivateState,
        persona: PersonaId,
        settings: &Settings,
        head: &ObjectHead,
        community: Option<&CommunityKey>,
    ) -> bool {
        if visibility_decision(store, private, persona, head, community).excluded {
            return false;
        }
        passes_non_block_filters(store, private, settings, head)
    }
}

fn passes_non_block_filters(
    store: &DurableStore,
    private: &PrivateState,
    settings: &Settings,
    head: &ObjectHead,
) -> bool {
    if settings.spam_filter_threshold > 0 && spam_score(head) >= settings.spam_filter_threshold {
        return false;
    }
    !private.filters.values().any(|filter| {
        if !filter.enabled {
            return false;
        }
        let value = filter.value.to_lowercase();
        match filter.kind {
            hydra_domain::LocalFilterKind::Word => format!(
                "{}\n{}",
                head.title.as_deref().unwrap_or_default(),
                head.body.as_str()
            )
            .to_lowercase()
            .contains(&value),
            hydra_domain::LocalFilterKind::Topic => head
                .communities
                .iter()
                .any(|community| community.as_str().eq_ignore_ascii_case(&value)),
            hydra_domain::LocalFilterKind::Thread => {
                head.anchor.as_str() == filter.value
                    || head
                        .root
                        .as_ref()
                        .is_some_and(|root| root.as_str() == filter.value)
            }
            hydra_domain::LocalFilterKind::Media => store.state().media.values().any(|media| {
                media.object == head.anchor
                    && (media.mime_type.to_lowercase().contains(&value)
                        || media.sha256.to_lowercase().contains(&value)
                        || media
                            .original_url
                            .as_deref()
                            .is_some_and(|url| url.to_lowercase().contains(&value))
                        || media
                            .blob_urls
                            .iter()
                            .any(|url| url.to_lowercase().contains(&value)))
            }),
            // Relay filters are enforced before network reads.
            hydra_domain::LocalFilterKind::Relay => false,
        }
    })
}

fn persona_public_key(store: &DurableStore, persona: PersonaId) -> Option<&NostrPublicKey> {
    store
        .state()
        .personas
        .get(persona)
        .map(|persona| &persona.public_key)
}

fn thread_involves_persona(
    store: &DurableStore,
    root: &AnchorId,
    public_key: &NostrPublicKey,
) -> bool {
    store.state().heads.current_heads().any(|head| {
        head.author == *public_key && (head.anchor == *root || head.root.as_ref() == Some(root))
    })
}

/// Evaluates the selected persona's effective block for one author and topic.
#[must_use]
pub fn block_decision(
    store: &DurableStore,
    private: &PrivateState,
    persona: PersonaId,
    author: &NostrPublicKey,
    community: Option<&CommunityKey>,
) -> BlockDecision {
    let Some(persona_record) = store.state().personas.get(persona) else {
        return uncertain_decision(
            "The active persona is unavailable.",
            None,
            Some(JudgmentExclusion::Block),
        );
    };
    let Ok(persona_key) = hydra_nostr::flocking_public_key(&persona_record.public_key) else {
        return uncertain_decision(
            "The active persona key is invalid.",
            None,
            Some(JudgmentExclusion::Block),
        );
    };
    let Ok(author_key) = hydra_nostr::flocking_public_key(author) else {
        return uncertain_decision(
            "The author's public key is invalid.",
            None,
            Some(JudgmentExclusion::Block),
        );
    };
    let config = private.flocking_profile.as_ref().map_or_else(
        || flocking_core::Config {
            version: flocking_core::CONFIG_VERSION.to_owned(),
            persona: persona_key.clone(),
            sources: Vec::new(),
            appearance_sources: BTreeSet::new(),
            local_pin_dismissals: Vec::new(),
        },
        |profile| profile.config.clone(),
    );
    if config.persona != persona_key {
        return uncertain_decision(
            "The judgment configuration belongs to another persona.",
            None,
            Some(JudgmentExclusion::Block),
        );
    }
    let source_states = private
        .flocking_profile
        .as_ref()
        .map_or(&[][..], |profile| profile.source_states.as_slice());
    let judgments = person_judgments(store, private, persona, &persona_key);
    let topic = match community {
        Some(community) => match flocking_core::Topic::parse(community.as_str()) {
            Ok(topic) => Some(topic),
            Err(_) => {
                return uncertain_decision(
                    "The community topic is invalid.",
                    None,
                    Some(JudgmentExclusion::Block),
                );
            }
        },
        None => None,
    };
    let context = flocking_core::Context { topic };
    let target = flocking_core::Target::Person(author_key);
    let evaluation = flocking_core::evaluate(
        flocking_core::EvaluationInput {
            config: &config,
            judgments: &judgments,
            source_states,
            context: &context,
        },
        flocking_core::Faculty::Block,
        &target,
    );
    let Ok(evaluation) = evaluation else {
        return uncertain_decision(
            "The effective block could not be evaluated.",
            None,
            Some(JudgmentExclusion::Block),
        );
    };
    decision_from_evaluation(
        evaluation,
        &persona_key,
        &target,
        &judgments,
        JudgmentExclusion::Block,
    )
}

/// Evaluates block and silence together for one concrete contribution.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one fail-closed boundary keeps visibility inputs and outcomes auditable"
)]
pub fn visibility_decision(
    store: &DurableStore,
    private: &PrivateState,
    persona: PersonaId,
    head: &ObjectHead,
    community: Option<&CommunityKey>,
) -> VisibilityDecision {
    let Some(persona_record) = store.state().personas.get(persona) else {
        return uncertain_decision("The active persona is unavailable.", None, None);
    };
    let Ok(persona_key) = hydra_nostr::flocking_public_key(&persona_record.public_key) else {
        return uncertain_decision("The active persona key is invalid.", None, None);
    };
    let Ok(author_key) = hydra_nostr::flocking_public_key(&head.author) else {
        return uncertain_decision("The author's public key is invalid.", None, None);
    };
    let Ok(event_id) = flocking_core::EventId::parse(head.anchor.as_str()) else {
        return uncertain_decision(
            "This contribution has no portable content identifier.",
            None,
            None,
        );
    };
    let config = private.flocking_profile.as_ref().map_or_else(
        || flocking_core::Config {
            version: flocking_core::CONFIG_VERSION.to_owned(),
            persona: persona_key.clone(),
            sources: Vec::new(),
            appearance_sources: BTreeSet::new(),
            local_pin_dismissals: Vec::new(),
        },
        |profile| profile.config.clone(),
    );
    if config.persona != persona_key {
        return uncertain_decision(
            "The judgment configuration belongs to another persona.",
            None,
            None,
        );
    }
    let source_states = private
        .flocking_profile
        .as_ref()
        .map_or(&[][..], |profile| profile.source_states.as_slice());
    let judgments = person_judgments(store, private, persona, &persona_key);
    let topic = match community {
        Some(community) => match flocking_core::Topic::parse(community.as_str()) {
            Ok(topic) => Some(topic),
            Err(_) => {
                return uncertain_decision("The community topic is invalid.", None, None);
            }
        },
        None => None,
    };
    let context = flocking_core::Context { topic };
    let person_target = flocking_core::Target::Person(author_key.clone());
    let logical_target = flocking_core::ContentTarget::Event(event_id);
    let content_target = flocking_core::Target::Content(logical_target.clone());
    let contribution = flocking_core::Contribution {
        author: author_key,
        target: logical_target,
        created_at: head.edited_at,
        first_seen: store.state().first_seen(head),
    };
    let result = flocking_core::evaluate_visibility(flocking_core::VisibilityInput {
        evaluation: flocking_core::EvaluationInput {
            config: &config,
            judgments: &judgments,
            source_states,
            context: &context,
        },
        contribution: &contribution,
    });
    let Ok(visibility) = result else {
        return uncertain_decision("Visibility could not be evaluated.", None, None);
    };
    match visibility.exclusion {
        Some(flocking_core::Exclusion::Block) => decision_from_evaluation(
            visibility.block,
            &persona_key,
            &person_target,
            &judgments,
            JudgmentExclusion::Block,
        ),
        Some(flocking_core::Exclusion::Silence {
            cutoff,
            local_timing_evidence,
        }) => {
            let mut decision = decision_from_evaluation(
                visibility.silence,
                &persona_key,
                &person_target,
                &judgments,
                JudgmentExclusion::Silence,
            );
            decision.cutoff = Some(cutoff);
            decision.local_timing_evidence = local_timing_evidence;
            decision
        }
        Some(flocking_core::Exclusion::Hide) => decision_from_evaluation(
            visibility.hide,
            &persona_key,
            &content_target,
            &judgments,
            JudgmentExclusion::Hide,
        ),
        Some(flocking_core::Exclusion::CommunityRemoval) => {
            let Some(membership) = visibility.membership else {
                return uncertain_decision(
                    "Community membership could not be evaluated.",
                    None,
                    Some(JudgmentExclusion::CommunityRemoval),
                );
            };
            decision_from_evaluation(
                membership,
                &persona_key,
                &content_target,
                &judgments,
                JudgmentExclusion::CommunityRemoval,
            )
        }
        None if visibility.eligible.is_none() => uncertain_decision(
            "Visibility is uncertain because judgment data is stale or missing.",
            None,
            None,
        ),
        None => VisibilityDecision::allowed(),
    }
}

fn person_judgments(
    store: &DurableStore,
    private: &PrivateState,
    persona: PersonaId,
    persona_key: &flocking_core::PublicKey,
) -> Vec<flocking_core::Judgment> {
    let mut judgments = store.state().flocking_judgments.clone();
    judgments.extend(
        private
            .flocking_judgments
            .values()
            .map(|record| record.judgment.clone()),
    );
    judgments.extend(
        store
            .state()
            .blocks
            .values()
            .filter(|item| item.persona == persona && item.blocked)
            .chain(private.blocks.values().filter(|item| item.blocked))
            .filter_map(|item| {
                let target = hydra_nostr::flocking_public_key(&item.target).ok()?;
                Some(flocking_core::Judgment {
                    author: persona_key.clone(),
                    faculty: flocking_core::Faculty::Block,
                    scope: flocking_core::Scope::Global,
                    target: flocking_core::Target::Person(target),
                    action: flocking_core::Action::Block,
                    created_at: item.changed_at,
                    event_id: None,
                    since: None,
                    reason: item.reason.clone(),
                    evidence: flocking_core::JudgmentEvidence::Local,
                })
            }),
    );
    judgments
}

fn decision_from_evaluation(
    evaluation: flocking_core::Evaluation,
    persona_key: &flocking_core::PublicKey,
    target: &flocking_core::Target,
    judgments: &[flocking_core::Judgment],
    exclusion: JudgmentExclusion,
) -> VisibilityDecision {
    match evaluation {
        flocking_core::Evaluation::Indeterminate { unknown, stale } => {
            let source = unknown.first().map(|state| state.source.to_string());
            uncertain_decision(
                if stale {
                    "Judgment status is uncertain because source data is stale or missing."
                } else {
                    "Judgment status is uncertain because source data is missing."
                },
                source,
                Some(exclusion),
            )
        }
        flocking_core::Evaluation::Determinate {
            effective: None, ..
        } => VisibilityDecision::allowed(),
        flocking_core::Evaluation::Determinate {
            effective: Some(effective),
            ..
        } => {
            let reason = flocking_core::canonical_current(judgments)
                .into_iter()
                .find(|judgment| {
                    judgment.author == effective.evidence.author
                        && judgment.faculty == effective.faculty
                        && judgment.scope == effective.scope
                        && &judgment.target == target
                        && judgment.action == effective.action
                        && judgment.event_id == effective.evidence.event_id
                })
                .and_then(|judgment| judgment.reason.clone());
            let inherited = &effective.evidence.author != persona_key;
            let source = inherited.then(|| effective.evidence.author.to_string());
            let scope = Some(effective.scope.to_string());
            let verb = match exclusion {
                JudgmentExclusion::Block => "Blocked",
                JudgmentExclusion::Silence => "Silenced",
                JudgmentExclusion::Hide => "Hidden",
                JudgmentExclusion::CommunityRemoval => "Removed",
            };
            let why = if effective.value {
                Some(if let Some(source) = &source {
                    format!("{verb} through {source}.")
                } else {
                    format!("{verb} by your direct judgment.")
                })
            } else {
                None
            };
            VisibilityDecision {
                excluded: effective.value,
                uncertain: effective.certainty == flocking_core::Certainty::Stale,
                exclusion: effective.value.then_some(exclusion),
                inherited,
                source,
                event_id: effective.evidence.event_id.map(|id| id.to_string()),
                reason,
                scope,
                why,
                cutoff: None,
                local_timing_evidence: false,
            }
        }
    }
}

fn uncertain_decision(
    why: &str,
    source: Option<String>,
    exclusion: Option<JudgmentExclusion>,
) -> VisibilityDecision {
    VisibilityDecision {
        excluded: true,
        uncertain: true,
        exclusion,
        inherited: source.is_some(),
        source,
        event_id: None,
        reason: None,
        scope: None,
        why: Some(why.to_owned()),
        cutoff: None,
        local_timing_evidence: false,
    }
}

#[must_use]
pub fn spam_score(head: &ObjectHead) -> u8 {
    let text = format!(
        "{}\n{}",
        head.title.as_deref().unwrap_or_default(),
        head.body.as_str()
    );
    let mut score = 0_u8;
    let link_count = text.matches("http://").count() + text.matches("https://").count();
    if link_count > 3 {
        let link_score = u8::try_from(((link_count - 3) * 10).min(45)).unwrap_or(45);
        score = score.saturating_add(link_score);
    }
    let letters = text.chars().filter(char::is_ascii_alphabetic).count();
    let uppercase = text.chars().filter(char::is_ascii_uppercase).count();
    if letters > 40 && uppercase * 4 > letters * 3 {
        score = score.saturating_add(30);
    }
    if text
        .chars()
        .scan(('\0', 0_u8), |(previous, run), character| {
            if character.eq_ignore_ascii_case(previous) {
                *run = run.saturating_add(1);
            } else {
                *previous = character;
                *run = 1;
            }
            Some(*run)
        })
        .any(|run| run >= 8)
    {
        score = score.saturating_add(25);
    }
    let content_lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if content_lines.iter().collect::<BTreeSet<_>>().len() + 2 < content_lines.len() {
        score = score.saturating_add(25);
    }
    score.min(100)
}

fn followed_authors(
    store: &DurableStore,
    private: &PrivateState,
    persona: PersonaId,
) -> BTreeSet<NostrPublicKey> {
    store
        .state()
        .follows
        .values()
        .filter(|item| item.persona == persona && item.following)
        .chain(private.follows.values().filter(|item| item.following))
        .map(|item| item.target.clone())
        .collect()
}

fn subscribed_topics(
    store: &DurableStore,
    private: &PrivateState,
    persona: PersonaId,
) -> BTreeSet<CommunityKey> {
    store
        .state()
        .subscriptions
        .values()
        .filter(|item| item.persona == persona && item.subscribed)
        .chain(
            private
                .subscriptions
                .values()
                .filter(|item| item.subscribed),
        )
        .map(|item| item.community.clone())
        .collect()
}

fn sort_feed(store: &DurableStore, heads: &mut [ObjectHead], lens: FeedLens) {
    match lens {
        FeedLens::Old => heads.sort_by_key(|head| (head.edited_at, head.anchor.clone())),
        FeedLens::Top => heads.sort_by_key(|head| {
            (
                Reverse(current_score(store, &head.anchor)),
                Reverse(head.edited_at),
                head.anchor.clone(),
            )
        }),
        FeedLens::Discussed => heads.sort_by_key(|head| {
            (
                Reverse(discussion_count(store, &head.anchor)),
                Reverse(head.edited_at),
                head.anchor.clone(),
            )
        }),
        FeedLens::Controversial => heads.sort_by_key(|head| {
            (
                Reverse(controversy(store, &head.anchor)),
                Reverse(head.edited_at),
                head.anchor.clone(),
            )
        }),
        _ => heads.sort_by_key(|head| (Reverse(head.edited_at), head.anchor.clone())),
    }
}

fn current_score(store: &DurableStore, target: &AnchorId) -> i64 {
    store
        .state()
        .reactions
        .iter()
        .filter(|reaction| reaction.target == *target)
        .map(|reaction| &reaction.actor)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|actor| store.state().current_stance(actor, target))
        .map(|value| match value {
            ReactionValue::Upvote => 1,
            ReactionValue::Downvote => -1,
            ReactionValue::Neutral | ReactionValue::Emoji(_) => 0,
        })
        .sum()
}

fn discussion_count(store: &DurableStore, target: &AnchorId) -> usize {
    store
        .state()
        .heads
        .current_heads()
        .filter(|head| head.root.as_ref() == Some(target))
        .count()
}

fn controversy(store: &DurableStore, target: &AnchorId) -> usize {
    let up = store
        .state()
        .reactions
        .iter()
        .filter(|item| item.target == *target && item.value == ReactionValue::Upvote)
        .count();
    let down = store
        .state()
        .reactions
        .iter()
        .filter(|item| item.target == *target && item.value == ReactionValue::Downvote)
        .count();
    up.min(down)
}

#[cfg(test)]
mod tests {
    use hydra_domain::{
        ContentBody, DurableEvent, FlockingJudgmentRecord, FlockingProfile, ObjectHead, Persona,
    };
    use tempfile::tempdir;

    use super::*;

    fn set_private_judgment(
        private: &mut PrivateState,
        persona: PersonaId,
        judgment: flocking_core::Judgment,
    ) {
        let record = FlockingJudgmentRecord {
            persona,
            public: false,
            judgment,
        };
        private
            .flocking_judgments
            .insert(record.judgment.address(), record);
    }

    #[test]
    fn spam_score_is_bounded_and_explainable_by_content() {
        let head = ObjectHead {
            anchor: AnchorId::parse("spam").unwrap(),
            author: NostrPublicKey::parse("author").unwrap(),
            kind: ObjectKind::Post,
            title: Some("BUY!!!!!!!!".to_owned()),
            body: ContentBody::parse(
                "HTTPS://A.TEST HTTPS://B.TEST HTTPS://C.TEST HTTPS://D.TEST\nSAME\nSAME\nSAME",
            )
            .unwrap(),
            communities: Vec::new(),
            root: None,
            parent: None,
            external_root: None,
            external_parent: None,
            external_source: None,
            edited_at: 1,
        };
        assert!(spam_score(&head) > 0);
        assert!(spam_score(&head) <= 100);
    }

    #[test]
    fn followed_block_exposes_provenance_and_hides_conservatively() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let persona_id = PersonaId::new();
        let persona_key = NostrPublicKey::parse(
            "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
        )
        .unwrap();
        let source_key = NostrPublicKey::parse(
            "c6047f9441ed7d6d3045406e95c07cd85a85ac7985c31c6346d2261a36e39c44",
        )
        .unwrap();
        let target_key = NostrPublicKey::parse(
            "f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9",
        )
        .unwrap();
        store
            .append(
                DurableEvent::PersonaCreated(Persona {
                    id: persona_id,
                    public_key: persona_key.clone(),
                    display_name: "Alice".to_owned(),
                    reddit_account: None,
                }),
                1,
            )
            .unwrap();
        let persona = hydra_nostr::flocking_public_key(&persona_key).unwrap();
        let source = hydra_nostr::flocking_public_key(&source_key).unwrap();
        let target = hydra_nostr::flocking_public_key(&target_key).unwrap();
        let judgment = flocking_core::Judgment {
            author: source.clone(),
            faculty: flocking_core::Faculty::Block,
            scope: flocking_core::Scope::Global,
            target: flocking_core::Target::Person(target),
            action: flocking_core::Action::Block,
            created_at: 2,
            event_id: Some(flocking_core::EventId::parse("1".repeat(64)).unwrap()),
            since: None,
            reason: Some("Repeated impersonation".to_owned()),
            evidence: flocking_core::JudgmentEvidence::FlockingEvent,
        };
        store
            .append(
                DurableEvent::RemoteEventReceived {
                    event_id: "1".repeat(64),
                    event_json: "{}".to_owned(),
                    heads: Vec::new(),
                    reactions: Vec::new(),
                    public_projections: Vec::new(),
                    flocking_judgments: vec![judgment],
                    community_appearances: Vec::new(),
                },
                2,
            )
            .unwrap();
        let private = PrivateState {
            flocking_profile: Some(FlockingProfile {
                persona: persona_id,
                config: flocking_core::Config {
                    version: flocking_core::CONFIG_VERSION.to_owned(),
                    persona,
                    sources: vec![flocking_core::Source {
                        pubkey: source.clone(),
                        grants: vec![flocking_core::FacultyGrant {
                            faculty: flocking_core::Faculty::Block,
                            global: true,
                            topics: BTreeSet::new(),
                            rank: Some(1),
                        }],
                        reverse_blocks: None,
                    }],
                    appearance_sources: BTreeSet::new(),
                    local_pin_dismissals: Vec::new(),
                },
                source_states: vec![flocking_core::SourceState {
                    source: source.clone(),
                    faculty: flocking_core::Faculty::Block,
                    scope: flocking_core::Scope::Global,
                    completeness: flocking_core::Completeness::Complete,
                }],
                appearance_complete_sources: BTreeSet::new(),
                changed_at: 2,
            }),
            ..PrivateState::default()
        };

        let decision = block_decision(&store, &private, persona_id, &target_key, None);

        assert!(decision.excluded);
        assert!(decision.inherited);
        assert_eq!(decision.source.as_deref(), Some(source.as_str()));
        assert_eq!(decision.reason.as_deref(), Some("Repeated impersonation"));
        assert!(!decision.uncertain);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario proves the complete temporal precedence sequence"
    )]
    fn silence_uses_signed_and_first_seen_time_while_block_still_dominates() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let persona_id = PersonaId::new();
        let persona_key = NostrPublicKey::parse(
            "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
        )
        .unwrap();
        let target_key = NostrPublicKey::parse(
            "f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9",
        )
        .unwrap();
        store
            .append(
                DurableEvent::PersonaCreated(Persona {
                    id: persona_id,
                    public_key: persona_key.clone(),
                    display_name: "Alice".to_owned(),
                    reddit_account: None,
                }),
                1,
            )
            .unwrap();
        let make_head = |character: char, edited_at| ObjectHead {
            anchor: AnchorId::parse(character.to_string().repeat(64)).unwrap(),
            author: target_key.clone(),
            kind: ObjectKind::Post,
            title: Some("Contribution".to_owned()),
            body: ContentBody::parse("Body").unwrap(),
            communities: Vec::new(),
            root: None,
            parent: None,
            external_root: None,
            external_parent: None,
            external_source: None,
            edited_at,
        };
        let receive = |store: &mut DurableStore, character: char, head, recorded_at| {
            store
                .append(
                    DurableEvent::RemoteEventReceived {
                        event_id: character.to_string().repeat(64),
                        event_json: "{}".to_owned(),
                        heads: vec![head],
                        reactions: Vec::new(),
                        public_projections: Vec::new(),
                        flocking_judgments: Vec::new(),
                        community_appearances: Vec::new(),
                    },
                    recorded_at,
                )
                .unwrap();
        };
        let old = make_head('a', 9);
        receive(&mut store, 'a', old.clone(), 9);
        let persona = hydra_nostr::flocking_public_key(&persona_key).unwrap();
        let target = hydra_nostr::flocking_public_key(&target_key).unwrap();
        let judgment = |faculty: flocking_core::Faculty,
                        action: flocking_core::Action,
                        character: char,
                        since: Option<u64>| flocking_core::Judgment {
            author: persona.clone(),
            faculty,
            scope: flocking_core::Scope::Global,
            target: flocking_core::Target::Person(target.clone()),
            action,
            created_at: 10,
            event_id: Some(
                flocking_core::EventId::parse(character.to_string().repeat(64)).unwrap(),
            ),
            since,
            reason: None,
            evidence: flocking_core::JudgmentEvidence::FlockingEvent,
        };
        store
            .append(
                DurableEvent::RemoteEventReceived {
                    event_id: "1".repeat(64),
                    event_json: "{}".to_owned(),
                    heads: Vec::new(),
                    reactions: Vec::new(),
                    public_projections: Vec::new(),
                    flocking_judgments: vec![judgment(
                        flocking_core::Faculty::Silence,
                        flocking_core::Action::Silence,
                        '1',
                        Some(10),
                    )],
                    community_appearances: Vec::new(),
                },
                10,
            )
            .unwrap();
        let new = make_head('b', 11);
        receive(&mut store, 'b', new.clone(), 11);
        let backdated = make_head('c', 1);
        receive(&mut store, 'c', backdated.clone(), 12);
        let private = PrivateState::default();

        assert!(!visibility_decision(&store, &private, persona_id, &old, None).excluded);
        let new_decision = visibility_decision(&store, &private, persona_id, &new, None);
        assert_eq!(new_decision.exclusion, Some(JudgmentExclusion::Silence));
        assert_eq!(new_decision.cutoff, Some(10));
        let backdated_decision =
            visibility_decision(&store, &private, persona_id, &backdated, None);
        assert_eq!(
            backdated_decision.exclusion,
            Some(JudgmentExclusion::Silence)
        );
        assert!(backdated_decision.local_timing_evidence);

        store
            .append(
                DurableEvent::RemoteEventReceived {
                    event_id: "2".repeat(64),
                    event_json: "{}".to_owned(),
                    heads: Vec::new(),
                    reactions: Vec::new(),
                    public_projections: Vec::new(),
                    flocking_judgments: vec![judgment(
                        flocking_core::Faculty::Block,
                        flocking_core::Action::Block,
                        '2',
                        None,
                    )],
                    community_appearances: Vec::new(),
                },
                13,
            )
            .unwrap();
        assert_eq!(
            visibility_decision(&store, &private, persona_id, &new, None).exclusion,
            Some(JudgmentExclusion::Block)
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario proves cross-faculty and cross-topic precedence"
    )]
    fn hide_and_membership_remain_independent_and_topic_relative() {
        let root = tempdir().unwrap();
        let mut store = DurableStore::open(root.path()).unwrap();
        let persona_id = PersonaId::new();
        let persona_key = NostrPublicKey::parse(
            "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
        )
        .unwrap();
        let author_key = NostrPublicKey::parse(
            "f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9",
        )
        .unwrap();
        store
            .append(
                DurableEvent::PersonaCreated(Persona {
                    id: persona_id,
                    public_key: persona_key.clone(),
                    display_name: "Alice".to_owned(),
                    reddit_account: None,
                }),
                1,
            )
            .unwrap();
        let head = ObjectHead {
            anchor: AnchorId::parse("a".repeat(64)).unwrap(),
            author: author_key,
            kind: ObjectKind::Post,
            title: Some("Cross-topic post".to_owned()),
            body: ContentBody::parse("Body").unwrap(),
            communities: vec![
                CommunityKey::parse("science").unwrap(),
                CommunityKey::parse("philosophy").unwrap(),
            ],
            root: None,
            parent: None,
            external_root: None,
            external_parent: None,
            external_source: None,
            edited_at: 2,
        };
        store
            .append(
                DurableEvent::NativeObjectChanged {
                    head: head.clone(),
                    outbound: Vec::new(),
                },
                2,
            )
            .unwrap();
        let persona = hydra_nostr::flocking_public_key(&persona_key).unwrap();
        let target = flocking_core::Target::Content(flocking_core::ContentTarget::Event(
            flocking_core::EventId::parse(head.anchor.as_str()).unwrap(),
        ));
        let judgment = |faculty, scope, action, created_at| flocking_core::Judgment {
            author: persona.clone(),
            faculty,
            scope,
            target: target.clone(),
            action,
            created_at,
            event_id: None,
            since: None,
            reason: None,
            evidence: flocking_core::JudgmentEvidence::Local,
        };
        let mut private = PrivateState::default();
        set_private_judgment(
            &mut private,
            persona_id,
            judgment(
                flocking_core::Faculty::CommunityMembership,
                flocking_core::Scope::Topic(flocking_core::Topic::parse("science").unwrap()),
                flocking_core::Action::Remove,
                3,
            ),
        );
        let science = CommunityKey::parse("science").unwrap();
        let philosophy = CommunityKey::parse("philosophy").unwrap();
        assert_eq!(
            visibility_decision(&store, &private, persona_id, &head, Some(&science)).exclusion,
            Some(JudgmentExclusion::CommunityRemoval)
        );
        assert!(
            !visibility_decision(&store, &private, persona_id, &head, Some(&philosophy)).excluded
        );
        assert!(!visibility_decision(&store, &private, persona_id, &head, None).excluded);

        set_private_judgment(
            &mut private,
            persona_id,
            judgment(
                flocking_core::Faculty::Hide,
                flocking_core::Scope::Global,
                flocking_core::Action::Hide,
                4,
            ),
        );
        assert_eq!(
            visibility_decision(&store, &private, persona_id, &head, Some(&science)).exclusion,
            Some(JudgmentExclusion::Hide)
        );
        set_private_judgment(
            &mut private,
            persona_id,
            judgment(
                flocking_core::Faculty::Hide,
                flocking_core::Scope::Global,
                flocking_core::Action::Unhide,
                5,
            ),
        );
        assert_eq!(
            visibility_decision(&store, &private, persona_id, &head, Some(&science)).exclusion,
            Some(JudgmentExclusion::CommunityRemoval)
        );
        set_private_judgment(
            &mut private,
            persona_id,
            judgment(
                flocking_core::Faculty::CommunityMembership,
                flocking_core::Scope::Topic(flocking_core::Topic::parse("science").unwrap()),
                flocking_core::Action::Restore,
                6,
            ),
        );
        assert!(!visibility_decision(&store, &private, persona_id, &head, Some(&science)).excluded);
    }
}
