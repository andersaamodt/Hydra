# Security and privacy

Hydra's user-facing data practices are defined in [`PRIVACY.md`](../PRIVACY.md),
and the Reddit adapter's exact access and retention boundary is documented in
[`REDDIT_DATA_API.md`](REDDIT_DATA_API.md).

Hydra assumes relays, Reddit responses, links, media, event bodies, and profile text are hostile input.

- Persona keys use the operating-system credential vault or Hydra’s encrypted permission-restricted fallback.
- Reddit OAuth credentials are persona-bound and never enter Nostr events, logs, backups, or the browser extension.
- The Firefox companion has no signing key and communicates through a narrow authenticated local bridge.
- Browsed Reddit bodies are transient and never enter public Nostr events.
- Official Reddit export import reads only `posts.csv` and `comments.csv`, applies strict size and record limits, and ignores every other export file.
- Public Nostr content is rendered as inert text; media is lazy, bounded, hash-checked, and sandboxed.
- Pseudonymous personas are not guaranteed anonymous because timing, relays, network paths, writing style, and user mistakes can correlate identities.
- Reddacted is a public one-way edit of the user’s own Hydra-originated Reddit projection, not encrypted secrecy.
- Local blocks and filters change only the user’s view and never claim to prevent another person from seeing public events.

Release testing covers malformed events, cyclic trees, oversized inputs, archive traversal, symlinks, OAuth callback confusion, extension-origin validation, projection duplication, edit divergence, retry safety, settings corruption, and persona isolation.
