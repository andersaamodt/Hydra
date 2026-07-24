# ADR 0001: Canonical Hydra 1.0 application architecture

Status: Accepted

## Decision

The public Hydra 1.0 specification is the product and architecture authority. Hydra uses a Rust-native core and a single lightweight, framework-free HTML/CSS/JavaScript interface inside Tauri 2. TypeScript and Electron are forbidden.

The former SwiftUI and GTK applications were non-canonical migration sources and were removed after parity was established. Release packaging, CI, and product claims use only the Tauri application.

The domain model remains UI-independent. Reddit remains isolated behind the generic projection boundary. The browser companion contains no Nostr private keys and communicates with the desktop application through a narrow authenticated channel.

Hydra uses Theurgy only at a real cross-project boundary: its runtime emits
Theurgy's generic typed snapshot, status, action-result, and operation-status
envelopes, and tests validate those actual outputs. Hydra does not maintain a
parallel Product IR, Surface IR, or runtime manifest because Tauri and the Hydra
runtime are the production application path.

Theurgy remains the owner of reusable institutional platform machinery such as
Developer ID signing, notarization, store submission, and publish-key handling.
It does not own Hydra's Nostr protocol, Reddit adapter, domain workflows, local
storage truth, feed policy, or UI semantics.

## Protocol audit

The Nostr registry was audited at upstream commit `db5fe3de8c5d1443b634c9bbf66ecb004f337057` on 2026-07-22. Experimental kinds `30800` and `30801` were unassigned at that revision and are retained provisionally for editable content heads and projection records respectively.

Reddit identity proofs are a Hydra-defined provider convention using NIP-39's extensible `platform:identity` form because NIP-39 does not define a Reddit-specific proof recipe.

## Consequences

- macOS, Linux, and Windows use one UI source without TypeScript.
- Runtime and protocol behavior remain testable without a window system.
- Platform-specific behavior is limited to Tauri capabilities, credential vaults, packaging, deep links, and accessibility integration.
- macOS and available Linux/Raspberry Pi builds receive hands-on automation before release; Windows support remains subject to the strongest available cross-checks when no Windows host is available.
- The canonical public specification replaces prior summaries.
- Product action legality has one owner in `hydra-runtime`; the Tauri host transports requests without a second allowlist.
- Feed and filtering policy has one owner in `hydra-lens`; the UI renders ordered, visibility-filtered projections.
- Theurgy validation is a required integration test, not a shadow application specification.
