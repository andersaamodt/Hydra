# Hydra 1.0 conformance

| Area | Public 1.0 status |
|---|---|
| Desktop/local-first core | Implemented in Rust with a Tauri 2 shell and framework-free JavaScript UI |
| Nostr personas and social graph | Implemented with standard Nostr facilities |
| `/h/` communities and multi-topic posts | Implemented |
| Editable posts and comments | Immutable anchors plus addressable Hydra heads |
| Voting, reaffirmation, Revisit, norms | Implemented |
| Open Nostr | Tagged discovery, explicit Uncategorized content, private categorization, and standard repost curation implemented |
| Messaging | Persona-addressed interoperable Nostr messages implemented |
| Reddit Bridge | OAuth, live transient browsing, optional projections, edits, divergence, and one-for-one voting implemented |
| Official Reddit export | Posts/comments-only preview, selective local or public import, exact source permalinks, and an in-app imported-writing library implemented |
| Big Stick and Reddacted | Restricted to Hydra-originated Reddit projections |
| Browser companion | Firefox-first, no keys, narrow local bridge |
| macOS/Linux | Native packaging and installed-app automation are release requirements |

The full gate is `./.tests/run`. Live Reddit OAuth testing requires Reddit-issued application credentials and uses only the dedicated QA account.
