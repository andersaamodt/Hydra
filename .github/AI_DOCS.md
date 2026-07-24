# Hydra contributor contract

Read `docs/AUTHORITATIVE_SPEC.md` before changing product behavior.

## Governing authorities

- Product behavior and architecture: `docs/AUTHORITATIVE_SPEC.md`.
- Product decisions not fully encoded by the specification: accepted ADRs under `docs/decisions/`.
- Domain invariants and workflow legality: Rust domain and application modules.
- Feed, ranking, blocking, local filtering, spam, and My Feed policy: `hydra-lens`.
- Reddit behavior: the `hydra-reddit` adapter behind the generic projection boundary.
- Runtime state and actions: `hydra-runtime`; native shells only transport typed requests.
- Tests: runnable entrypoints under `.tests/`, plus local unit tests beside their arche.

## Local standards decision

- Audited against Phronesis revision `aadb70065971d7b65995da2df118477d353ca407` on 2026-07-22.
- Re-audit when Phronesis changes runtime-boundary, storage, GUI, tests, or parsimony standards.
- Use no TypeScript.
- Keep runtime data, credentials, caches, logs, screenshots, generated platform sources, and build products outside the repository.
- Prefer typed states that make invalid transitions unrepresentable.
- Do not add speculative abstractions, compatibility paths, feature flags, or services.

## Theurgy boundary

- Hydra directly implements its product runtime and Tauri UI; it does not maintain unused Theurgy Product IR, Surface IR, or runtime manifests.
- Hydra emits the generic Theurgy snapshot, runtime-status, action-result, and operation-status envelopes and validates real runtime output against Theurgy revision `3486ee925a2d5ca905e98b1f4f88b3f6bf35d3c2`.
- Theurgy owns reusable signing, notarization, store submission, and other institutional platform machinery when Hydra adopts those workflows.
- Theurgy does not own Hydra personas, Nostr semantics, Reddit workflows, feeds, storage truth, or UI behavior.

## Negative space

- The UI does not rank, block, classify spam, decide workflow order, or infer private persona relationships.
- The Tauri host does not duplicate runtime action policy.
- The browser companion never receives a Nostr private key.
- Flock and Circles hooks do not authorize speculative 1.0 implementations.

Behavior changes require an ADR and an authoritative-spec update before implementation.
