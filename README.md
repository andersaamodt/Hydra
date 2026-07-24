# Hydra

Hydra is the underground Reddit alternative: a desktop-first, local-first Nostr app where communities, relationships, writing, and memory survive any one platform.

Hydra 1.0 provides ownerless `/h/` topic communities, pseudonymous personas, editable posts and nested comments, voting and reaffirmation, private Revisit workflows, emergent norms, Nostr messaging, Open Nostr discovery, and an optional Reddit projection bridge.

Reddit-specific behavior is isolated behind an adapter. Browsed Reddit bodies remain transient. Users can post, comment, edit, and vote through the bridge, import their own writing from Reddit’s official account-data export with exact source permalinks intact, and use Big Stick or Reddacted on Reddit projections that originated in Hydra.

There is no Hydra-hosted social website, central index, moderator class, global karma, or required Hydra server.

## Build and test

Hydra uses stable Rust, framework-free JavaScript, and Tauri 2. It contains no TypeScript or Electron.

```sh
./.tests/run
```

Build platform packages with:

```sh
tools/package/build-macos-app
tools/package/build-linux-deb
```

Local macOS artifacts are ad-hoc signed. Public Developer ID signing and notarization require release credentials.

## Architecture

- `crates/hydra-domain`: framework-free identities, communities, reactions, memory, and state machines.
- `crates/hydra-store`: checksummed append log, encrypted private records, drafts, settings, and media.
- `crates/hydra-nostr`: standard Nostr composition and the two minimal Hydra protocol additions.
- `crates/hydra-reddit`: detachable OAuth, browsing, projection, vote, Big Stick, Reddacted, and official export adapter.
- `crates/hydra-app`: domain orchestration without desktop dependencies.
- `crates/hydra-runtime`: typed actions and Theurgy-compatible envelopes.
- `apps/desktop/tauri`: the desktop shell.
- `hydra-ui`: the shared nontechnical interface.
- `extensions/firefox`: a narrow companion with no Nostr keys.
- `protocol`: public Hydra event schemas and vectors.

The [public specification](docs/AUTHORITATIVE_SPEC.md) governs 1.0 behavior.

## Privacy

Public Nostr events are public. Local categorization, private lists, drafts, Revisit entries, and credentials remain local or encrypted. Pseudonymous personas are not guaranteed anonymous.

Hydra stores ordinary state under `~/hydra` by default or `HYDRA_HOME`. Repository directories never contain app state or user data.

## License

Hydra is free software under AGPL-3.0-or-later. Protocol schemas and vectors are freely reusable for interoperable implementations.
