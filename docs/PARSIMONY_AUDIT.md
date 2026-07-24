# Phronesis parsimony audit

This is derivative evidence for `docs/AUTHORITATIVE_SPEC.md`, audited against Phronesis `28af77100a6f1c3cda002e80429ba12bd7f0fce4` on 2026-07-23. Re-audit when the authoritative specification or Phronesis parsimony, GUI, runtime-boundary, storage, or test standards change.

## Arches

- Product intent: `docs/AUTHORITATIVE_SPEC.md`.
- Domain legality and normalization: `hydra-domain`.
- Public event meaning: `hydra-nostr` and the schemas under `protocol/`.
- Durable truth: the encrypted, checksummed append log in `hydra-store`.
- Feed, ranking, blocking, and spam policy: `hydra-lens`.
- Workflow truth and action dispatch: `hydra-app` and `hydra-runtime`.
- Reddit behavior: the detachable `hydra-reddit` adapter.
- UI: a projection of runtime state; it owns no second policy engine.

## Necessary surfaces

Hydra keeps one domain core, one append-only store, one Nostr transport, one generic projection boundary, one detachable Reddit adapter, one desktop shell, and one narrow Firefox companion. The small archive, media, messaging, and lens crates each own a distinct required contract rather than providing speculative indirection.

The public 1.0 adds only two custom Nostr event families: editable content heads and public projection records. Identity, profiles, forum anchors, comments, follows, lists, reactions, labels, messages, relay metadata, reconciliation, external identifiers, reposts, and media metadata reuse current Nostr standards.

My Feed, `/h/`, and Open Nostr are distinct user spaces backed by the same event truth. Reddit is an optional projection capability; removing its adapter leaves the complete Nostr application.

Stonr uses the ordinary Nostr transport plus one declarative relay-capability profile. It introduces no Stonr-specific runtime path.

Theurgy owns the generic runtime envelope contract and future institutional release machinery. It does not own Hydra’s social policy, domain model, storage, ranking, Nostr interpretation, or Reddit adapter.

## Negative space

- No hosted Hydra social service, central index, global moderator, global karma, analytics identity, scraper, or required relay.
- No UI-local workflow state machine and no Tauri-host policy copy.
- No public Flock, Gloss, Circles, local-LLM, or mobile implementation in 1.0.
- No restoration transition after Reddacted withdrawal.
- No transient Reddit body is silently converted into a public Hydra record.

## Simplification result

The audit removed duplicate local READMEs, a dead narrow-window layout, decorative capitalization and button motion, an unsupported network-routing claim, and a false Reddacted restoration transition. The later anti-AI-GUI pass also removed permanent product explanations, title/subtitle duplication, boxed empty states, decorative gradients and blur, pill inflation, selection rails, and nested card chrome while retaining accessible focus outlines and task-specific help. It added no new service or framework; the only compatibility addition is a declarative Stonr profile with a signed-publication test.
