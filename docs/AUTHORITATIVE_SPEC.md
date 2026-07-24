# Hydra 1.0 Product and Protocol Specification

This document is authoritative for the public Hydra 1.0 implementation.

## Constitution

- Hydra is a desktop-first, local-first Nostr client.
- Hydra communities are ownerless topic spaces written `/h/<topic>`.
- People control their own personas, follows, filters, blocks, subscriptions, and ranking lenses.
- Hydra has no global moderator class, global karma, required server, or hosted social website.
- Public Nostr data is public; local-only data is never silently published.
- Reddit is an optional projection adapter and never part of Hydra’s domain core.

## Nostr

- A persona is one Nostr keypair.
- Posts use interoperable immutable anchors plus addressable editable heads.
- Comments use NIP-22 anchors plus addressable editable heads.
- Posts may carry several topic tags and retain one discussion tree.
- Hydra reuses standard Nostr follows, lists, reactions, labels, messages, relay metadata, external identifiers, remote signing, and media metadata.
- Open Nostr shows tagged and untagged public discussion from selected relays.
- Tagged events participate naturally in matching `/h/` topics.
- Untagged events remain explicitly Uncategorized.
- A private categorization changes only one persona’s local view.
- Public curation uses a standard repost with topic tags and preserves original authorship.

## Reddit Bridge

- Reddit code lives only in the detachable Reddit adapter.
- OAuth is preferred and credentials remain in the local credential vault.
- Browsed Reddit bodies are transient and are not published to Nostr.
- Hydra stores only the identifiers, hashes, and state required to maintain a user-requested projection.
- Users may explicitly project Hydra posts, comments, edits, and one-for-one vote actions to Reddit.
- Crossposting is off by default and may be configured globally, per persona, per content kind, or per community.
- A failed Reddit action never destroys the Hydra original.
- Big Stick may attach a portable Nostr record only to a Reddit copy that originated in Hydra.
- Reddacted may permanently withdraw only a Reddit copy that originated in Hydra.
- Reddacted is terminal in Hydra 1.0 and has no restoration action.
- Hydra can import the user’s posts and comments from Reddit’s official account-data export.
- The import ignores every other file in the export.
- Imported writing is local-only unless the user explicitly chooses to publish it to Nostr.
- Imported posts and comments remain visibly distinct from Hydra-originated writing and retain their exact Reddit source permalinks.

## Social and memory systems

- Votes are Nostr reactions with current, flattened, and reaffirmation-aware views.
- Changing current stance is immediate; repeated same-valence affirmation is credited at most once per 18 hours.
- Revisit is private, persona-bound memory and is separate from voting.
- Norms are signed propositions with voluntary endorsement or divergence and no removal power.
- Direct messages use interoperable Nostr messaging and remain persona-addressed.
- Counts are quietly available and never become a global karma score.

## Security and release requirements

- The extension never receives a Nostr private key.
- All external strings, events, media, and URLs are untrusted and rendered inertly.
- Local event storage is append-only, checksum-verified, replayable, and exportable.
- Outbound publication is queued, retry-safe, and idempotent.
- Every release must pass Rust unit/integration tests, UI tests, protocol tests, extension tests, packaging checks, and hands-on installed-app automation where the platform is available.
- Repository paths never contain app state, credentials, private keys, OAuth tokens, or user data.

## Future compatibility

Hydra 1.0 keeps explicit module boundaries for local semantic recommendations, Flock lenses and coordination, Gloss aliases, mobile clients, and voluntary community economies. Those systems are not part of the public 1.0 behavior.
