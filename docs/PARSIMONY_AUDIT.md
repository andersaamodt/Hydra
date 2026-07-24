# Phronesis parsimony audit

Hydra keeps one domain core, one append-only store, one Nostr transport, one detachable Reddit adapter, one desktop shell, and one narrow Firefox companion.

Theurgy owns typed runtime envelopes and transport contracts. It does not own Hydra’s social policy, domain model, storage, ranking, or platform adapters.

The public 1.0 adds only two custom Nostr event families: editable content heads and public projection records. General Nostr discovery and curation reuse standard events.

The UI exposes three clear spaces:

- My Feed for chosen relationships and places;
- `/h/` for ownerless topic communities;
- Open Nostr for the wider uncategorized and topic-tagged commons.

Reddit remains an optional capability. Removing its adapter leaves a complete Nostr application.
