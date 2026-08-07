# Hydra 1.0 conformance

This file is an evidence map, not a second product specification. Product meaning comes from `docs/AUTHORITATIVE_SPEC.md`; domain and protocol tests prove the executable interpretation.

| Authoritative area | Governing implementation | Direct evidence |
|---|---|---|
| Desktop/local-first core | `hydra-store`, `hydra-runtime`, `apps/desktop/tauri` | replay, corruption, passive-read, bridge, and packaging tests in `./.tests/run` |
| Personas, custody, follows, lists, and relays | `hydra-app`, `hydra-nostr`, `hydra-store` | persona isolation, key-vault, NIP-02, NIP-51, NIP-65, NIP-46, and backup unit tests |
| `/h/`, multi-topic posts, and nested discussion | `hydra-domain`, `hydra-app`, `hydra-nostr` | one-tree, edit-lineage, root/parent, and multi-community unit tests |
| Editable posts and comments | `hydra-nostr` content-head schema | replacement, wrong-author, malformed-head, and stable-anchor tests plus `protocol/vectors/` |
| Voting, reaffirmation, Revisit, lenses, norms, and blocks | `hydra-app`, `hydra-lens`, `hydra-nostr` | reaction-history, 18-hour credit, encrypted memory, deterministic lens, label, and list tests |
| Open Nostr | `hydra-nostr`, `hydra-runtime` | tagged/Uncategorized discovery, bounded feed, private categorization, and standard repost tests |
| Relay interoperability | ordinary `hydra-nostr` transport and `support/stonr/hydra-support.yaml` | `.tests/stonr` validates the capability profile and publishes a real signed Hydra post through a live local Stonr relay |
| Messaging | `hydra-messaging`, `hydra-nostr`, `hydra-app` | NIP-17 sender/receiver wraps, encrypted recovery, request filtering, and persona-bound tests |
| Reddit Bridge | `hydra-reddit` behind `hydra-projection` | OAuth-state, transient parser, exact-parent, idempotency, divergence, adaptive-state, and failure tests |
| Official Reddit export | `hydra-reddit::export` | ZIP traversal defense and posts/comments-only tests plus UI source-link contract |
| Big Stick and Reddacted | `hydra-reddit::bridge` | verified-before-edit tests, Hydra-origin restriction, and terminal-withdrawal tests |
| Firefox companion | `extensions/firefox` | manifest, CSP, permissions, URL validation, native-message, and no-key checks in `.tests/firefox` |
| Security and adversarial behavior | all owning modules | `.tests/adversarial`, hostile-width/depth, symlink, oversized input, checksum, and unknown-schema tests |
| Cross-platform release | Tauri shell and package scripts | macOS app/DMG, Linux ARM64 DEB, and Windows NSIS jobs in `.github/workflows/ci.yml` |

The canonical local gate is `./.tests/run`. GitHub Actions provides the additional clean-host package evidence.

Two distribution checks require external authority rather than more code: live Reddit OAuth needs a Reddit-issued client ID and public macOS distribution needs Developer ID/notarization credentials. Hydra’s tests use fakes and never a personal Reddit account.
