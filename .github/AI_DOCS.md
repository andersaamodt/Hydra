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

- Audited against Phronesis revision `28af77100a6f1c3cda002e80429ba12bd7f0fce4` and Wizardry Apps revision `1b174ebe7a30dec09bc97158ebe9d72aa041975d` on 2026-07-23.
- Re-audit when Phronesis changes runtime-boundary, storage, GUI, tests, or parsimony standards.
- Use no TypeScript.
- Keep runtime data, credentials, caches, logs, screenshots, generated platform sources, and build products outside the repository.
- Prefer typed states that make invalid transitions unrepresentable.
- Do not add speculative abstractions, compatibility paths, feature flags, or services.

## Local exceptions

- Hydra’s green light/dark palette is an app-identity palette, centralized in `hydra-ui/styles.css`; it does not import a second runtime theme catalog.
- Tauri requires its generated schemas and staged sidecar beneath `apps/desktop/tauri` while building. Those disposable paths are the only additions to the canonical Phronesis `.gitignore` and are never tracked.
- The encrypted append log is machine-owned JSONL because it is a signed-event ledger, not a user-edited preference document.

## Theurgy boundary

- Hydra directly implements its product runtime and Tauri UI; it does not maintain unused Theurgy Product IR, Surface IR, or runtime manifests.
- Hydra emits the generic Theurgy snapshot, runtime-status, action-result, and operation-status envelopes and validates real runtime output against Theurgy revision `5b92846e118c486401e61c21984322a4c19c9e10`.
- Theurgy owns reusable signing, notarization, store submission, and other institutional platform machinery when Hydra adopts those workflows.
- Theurgy does not own Hydra personas, Nostr semantics, Reddit workflows, feeds, storage truth, or UI behavior.

## Negative space

- The UI does not rank, block, classify spam, decide workflow order, or infer private persona relationships.
- The Tauri host does not duplicate runtime action policy.
- The browser companion never receives a Nostr private key.
- Flock and Circles hooks do not authorize speculative 1.0 implementations.
- Stonr uses `support/stonr/hydra-support.yaml`; `.tests/stonr` verifies the exact relay capabilities when a Stonr binary is available.

## Release boundaries

- `.tests/run` is the canonical local gate; GitHub Actions additionally packages macOS, Linux ARM64, and Windows.
- Public macOS distribution still requires an external Developer ID and notarization credentials.
- Live Reddit OAuth remains unavailable until Reddit issues an application client ID; tests use fakes and never a personal Reddit account.

Behavior changes require an ADR and an authoritative-spec update before implementation.
