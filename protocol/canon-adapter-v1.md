# Canon adapter v1

Hydra interoperates with Book Club through signed public Nostr events. It does
not call a Book Club API, read Book Club storage, or copy Canon records into
Hydra's native object model.

## Accepted boundary

The adapter recognizes NIP-78 kind `30078` only when all of these are true:

- the event signature and identifier are valid;
- `L` is `dev.wizardry.canon`;
- `version` is exactly `2`;
- one namespaced `l` tag identifies a supported Canon role;
- `d` is `dev.wizardry.canon:<role>:<object-id>`;
- the JSON object agrees with that identifier; and
- no recursively nested password, key, token, authorization, credential, or
  secret field appears in public content.

Hydra derives a bounded preview of title, creators, identifiers, and summary.
The signed event remains the evidence. Unsupported versions and malformed
Canon records are never materialized.

Hydra may also retain verified standard reading events without translating
them: NIP-94 file metadata, NIP-84 highlights, NIP-51 curation sets, and
NIP-52 date/time events. Existing Hydra support owns NIP-22 comments and
NIP-25 reactions.

## Cross-app actions

- `Open in Book Club` passes a NIP-19 event entity through a
  `bookclub://nostr/…` handler.
- `hydra://nostr?uri=nostr:…` resolves a portable event from its relay hints
  and configured read relays. Resolving is transient.
- `Keep locally` explicitly stores the verified raw event as local evidence.
- `Discuss in Hydra` publishes a NIP-22 comment whose NIP-73 `I/i` identifier
  and `K/k` type match the work identifier used by Book Club.

Direct Book Club cross-links are a default-on local preference. Hydra shows
them only when the desktop shell finds a registered `bookclub:` handler.
Turning them off hides app-specific handoffs without disabling Canon or
standard Nostr parsing.

Browsing, resolving, and previewing never publish or persist. Neither app
receives the other's private keys, relay credentials, drafts, queues, local
preferences, private groups, or room-control state.

## Ownership

Book Club's Canon v2 contract is the vocabulary authority. This file defines
only Hydra's adapter boundary. Protocol changes must remain standard-first,
versioned, one-way migratable, and independently verifiable.
