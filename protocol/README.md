# Hydra protocol

Hydra is a Nostr client with two small application conventions:

- addressable editable heads over interoperable immutable post/comment anchors;
- addressable public records mapping a Hydra object to its user-created external projection.

Identity, follows, lists, comments, reactions, labels, encrypted messages, external references, relay declarations, reconciliation, reposts, media metadata, portable identifiers, and remote signing reuse existing Nostr NIPs.

The protocol requires no Hydra server or private API. Numeric experimental kinds must be rechecked against the public Nostr kind registry before standardization.
