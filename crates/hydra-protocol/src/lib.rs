#![forbid(unsafe_code)]
//! Hydra's documented Nostr mappings and minimal custom event registry.
//!
//! Event construction and relay I/O live in `hydra-nostr`; this crate is the
//! dependency-free protocol contract that other clients can implement.

use serde::{Deserialize, Serialize};

/// Experimental addressable kind for editable Hydra object heads.
///
/// Unassigned in the official Nostr registry at audited upstream commit
/// `db5fe3de8c5d1443b634c9bbf66ecb004f337057` on 2026-07-22. Recheck before
/// publication as a NIP.
pub const OBJECT_HEAD_KIND: u16 = 30_800;

/// Experimental addressable kind for external projection records.
pub const PROJECTION_RECORD_KIND: u16 = 30_801;

/// Experimental addressable kind for one person's current community-color choice.
/// Unlisted in the official machine-readable Nostr kind registry when checked on 2026-08-21.
pub const COMMUNITY_COLOR_SCHEME_KIND: u16 = 30_802;

pub const PROTOCOL_VERSION: &str = "hydra-protocol/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventRole {
    ForumAnchor,
    CommentAnchor,
    Reaction,
    ExternalReaction,
    EditableHead,
    ProjectionRecord,
    CommunityColorScheme,
    FollowList,
    InterestList,
    MuteList,
    InboxRelayList,
    Label,
    DirectMessageWrap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventMapping {
    pub role: EventRole,
    pub kind: u16,
    pub standard_nip: Option<u16>,
    pub addressable: bool,
}

pub const EVENT_MAPPINGS: &[EventMapping] = &[
    EventMapping {
        role: EventRole::ForumAnchor,
        kind: 11,
        standard_nip: Some(7),
        addressable: false,
    },
    EventMapping {
        role: EventRole::CommentAnchor,
        kind: 1111,
        standard_nip: Some(22),
        addressable: false,
    },
    EventMapping {
        role: EventRole::Reaction,
        kind: 7,
        standard_nip: Some(25),
        addressable: false,
    },
    EventMapping {
        role: EventRole::ExternalReaction,
        kind: 17,
        standard_nip: Some(25),
        addressable: false,
    },
    EventMapping {
        role: EventRole::EditableHead,
        kind: OBJECT_HEAD_KIND,
        standard_nip: None,
        addressable: true,
    },
    EventMapping {
        role: EventRole::ProjectionRecord,
        kind: PROJECTION_RECORD_KIND,
        standard_nip: None,
        addressable: true,
    },
    EventMapping {
        role: EventRole::CommunityColorScheme,
        kind: COMMUNITY_COLOR_SCHEME_KIND,
        standard_nip: None,
        addressable: true,
    },
    EventMapping {
        role: EventRole::FollowList,
        kind: 3,
        standard_nip: Some(2),
        addressable: false,
    },
    EventMapping {
        role: EventRole::InterestList,
        kind: 10015,
        standard_nip: Some(51),
        addressable: false,
    },
    EventMapping {
        role: EventRole::MuteList,
        kind: 10000,
        standard_nip: Some(51),
        addressable: false,
    },
    EventMapping {
        role: EventRole::InboxRelayList,
        kind: 10050,
        standard_nip: Some(17),
        addressable: false,
    },
    EventMapping {
        role: EventRole::Label,
        kind: 1985,
        standard_nip: Some(32),
        addressable: false,
    },
    EventMapping {
        role: EventRole::DirectMessageWrap,
        kind: 1059,
        standard_nip: Some(59),
        addressable: false,
    },
];

#[must_use]
pub fn mapping(role: EventRole) -> Option<&'static EventMapping> {
    EVENT_MAPPINGS.iter().find(|mapping| mapping.role == role)
}

#[must_use]
pub fn custom_kind(kind: u16) -> bool {
    matches!(
        kind,
        OBJECT_HEAD_KIND | PROJECTION_RECORD_KIND | COMMUNITY_COLOR_SCHEME_KIND
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_role_and_custom_kind_is_unique() {
        let roles = EVENT_MAPPINGS
            .iter()
            .map(|item| item.role)
            .collect::<BTreeSet<_>>();
        let custom = EVENT_MAPPINGS
            .iter()
            .filter(|item| item.standard_nip.is_none())
            .map(|item| item.kind)
            .collect::<BTreeSet<_>>();
        assert_eq!(roles.len(), EVENT_MAPPINGS.len());
        assert_eq!(custom.len(), 3);
    }

    #[test]
    fn addressable_custom_kinds_use_the_nip_01_range() {
        for mapping in EVENT_MAPPINGS.iter().filter(|item| item.addressable) {
            assert!((30_000..40_000).contains(&mapping.kind));
        }
    }
}
