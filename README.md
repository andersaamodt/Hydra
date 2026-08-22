# Hydra

Hydra is the underground Reddit alternative: a desktop-first, local-first Nostr app where communities, relationships, writing, and memory survive any one platform.

Hydra 1.0 provides ownerless `/h/` topic communities, pseudonymous personas, editable posts and nested comments, voting and reaffirmation, private Revisit workflows, emergent norms, Nostr messaging, Open Nostr discovery, and an optional Reddit projection bridge.

Reddit-specific behavior is isolated behind an adapter. Browsed Reddit bodies remain transient. Users can post, comment, and edit through the bridge, import their own writing from Reddit’s official account-data export with exact source permalinks intact, and use Big Stick or Reddacted on Reddit projections that originated in Hydra. Reddit vote projection is deferred pending policy clarification; Nostr voting remains available.

Canon reading records are likewise handled by a narrow public-protocol adapter.
Hydra can preview verified Book Club records, discuss works through standard
NIP-22/NIP-73 identifiers, retain the signed source event on request, and hand
portable `nostr:` references to Book Club. It does not share Book Club storage
or translate Canon records into Hydra-owned objects. See
[the Canon adapter contract](protocol/canon-adapter-v1.md).

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

Local macOS artifacts use Forge's dedicated local signing identity when it is
available, which preserves Keychain authorization across rebuilds. They fall
back to ad-hoc signing otherwise. Public Developer ID signing and notarization
require release credentials.

Hydra includes `wizardry.workspace.conf` for App Forge. Import the project folder into Forge to build and install the native macOS app; its canonical square icon master and generated platform assets live under `assets/`.
Forge reads `app-blueprint/app.ir.yaml` only for native bundle metadata; the Tauri configuration and Rust sidecar remain Hydra's canonical application architecture.
Hydra honors `HYDRA_MACOS_SIGNING_IDENTITY` first and Forge's
`WIZARDRY_CODESIGN_IDENTITY` second when an explicit signing identity is needed.

For Firefox development, load `extensions/firefox/manifest.json` as a temporary add-on from `about:debugging`. The companion only opens Reddit objects in Hydra, compacts visible Hydra markers, and sends narrowly validated native messages; it never receives Nostr keys or Reddit credentials.

When Stonr is installed, the canonical test gate also launches a local relay and verifies a real signed publication. Relay operators can apply `support/stonr/hydra-support.yaml` to lock Hydra’s required capabilities.

## Architecture

- `crates/hydra-domain`: framework-free identities, communities, reactions, memory, and state machines.
- `crates/hydra-store`: checksummed append log, encrypted private records, drafts, settings, and media.
- `crates/hydra-nostr`: standard Nostr composition, Hydra's minimal custom protocol additions, and the projection-only Canon adapter.
- `crates/hydra-reddit`: detachable OAuth, browsing, projection, Big Stick, Reddacted, and official export adapter.
- `crates/hydra-app`: domain orchestration without desktop dependencies.
- `crates/hydra-runtime`: typed actions and Theurgy-compatible envelopes.
- `apps/desktop/tauri`: the desktop shell.
- `hydra-ui`: the shared nontechnical interface.
- `extensions/firefox`: a narrow companion with no Nostr keys.
- `protocol`: public Hydra event schemas and vectors.

The [public specification](docs/AUTHORITATIVE_SPEC.md) governs 1.0 behavior.
The [Flocking integration](docs/FLOCKING_INTEGRATION.md) supplies voluntary,
inspectable community-shaping judgments without creating a moderator class.

## Privacy

Public Nostr events are public. Local categorization, private lists, drafts, Revisit entries, and credentials remain local or encrypted. Pseudonymous personas are not guaranteed anonymous.

Hydra stores ordinary state under `~/hydra` by default or `HYDRA_HOME`. Repository directories never contain app state or user data.

Read the full [privacy policy](PRIVACY.md), [security model](docs/SECURITY.md),
and [Reddit Data API use and retention statement](docs/REDDIT_DATA_API.md).

## License

Hydra is free software under AGPL-3.0-or-later. Protocol schemas and vectors are freely reusable for interoperable implementations.
