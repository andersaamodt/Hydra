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
            .filter(|head| Self::visible(store, private, persona, settings, head))
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
            .filter(|head| Self::visible(store, private, persona, settings, head))
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
        if blocked_authors(store, private, persona).contains(&head.author) {
            return false;
        }
        if settings.spam_filter_threshold > 0 && spam_score(head) >= settings.spam_filter_threshold
        {
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

fn blocked_authors(
    store: &DurableStore,
    private: &PrivateState,
    persona: PersonaId,
) -> BTreeSet<NostrPublicKey> {
    store
        .state()
        .blocks
        .values()
        .filter(|item| item.persona == persona && item.blocked)
        .chain(private.blocks.values().filter(|item| item.blocked))
        .map(|item| item.target.clone())
        .collect()
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
    use hydra_domain::{ContentBody, ObjectHead};

    use super::*;

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
}
