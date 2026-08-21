# Flocking integration

## Purpose

Hydra is the first adopter of the independent Flocking specification and Rust
libraries. Flocking remains the semantic authority for judgment evaluation;
Hydra supplies storage, relay access, signing, application policy, and the
interface.

The integration should feel like ordinary Hydra behavior. People remove, block,
silence, hide, restore, pin, and follow judgments in place; Hydra explains
inherited results through concise provenance instead of presenting Flocking as
a separate institution.

## Boundaries

- `flocking-core` is a pinned Hydra domain dependency for configuration,
  judgment selection, precedence, visibility, pins, Reverse Flocking, and
  Rescue.
- `flocking-nostr` is a pinned `hydra-nostr` dependency for canonical judgment
  events and faithful NIP-02/NIP-51 compatibility.
- `hydra-domain` owns only Hydra identities and durable host records; it does
  not reimplement Flocking evaluation.
- `hydra-store` persists authored judgments, signed source events, first-seen
  evidence, relay completeness, and encrypted persona-local configuration.
- `hydra-app` assembles evaluator inputs and coordinates local or published
  actions without copying inherited judgments into authored state.
- `hydra-runtime` exposes stable Hydra-shaped actions and provenance views to
  the interface.
- `hydra-ui` presents the concrete actions where they are useful and offers a
  small `Why?` explanation for inherited results.
- Relay fetching, signing, storage, placeholders, reveals, and interface copy
  remain Hydra policy and never enter the Flocking libraries.

Hydra will not add a `hydra-flocking` wrapper crate unless a demonstrated
boundary cannot remain simple with this composition.

## Identity mappings

- A Hydra `CommunityKey` maps exactly to a Flocking bare `Topic`.
- A Hydra `NostrPublicKey` maps exactly to a Flocking person target.
- An immutable Hydra anchor maps to its Nostr event ID.
- An editable Hydra object maps to its stable Nostr address coordinate rather
  than its latest revision event.
- `/h/science` and `/r/science` remain projections of the bare topic `science`.

Mapping tests must fail closed when a Hydra object lacks the portable identity
required by a judgment.

## State

Each persona has one encrypted Flocking configuration containing sources,
per-faculty ranks, enabled global/topic scopes, Reverse Flocking scopes, and
local inherited-pin dismissals. Public disclosure of this configuration is not
required.

Hydra stores direct local judgments separately from published signed judgments.
Inbound canonical events retain their event ID, author, first-seen time, relay
evidence, and parsing result. Source input remains explicitly complete, stale,
or unknown; missing relay data never becomes an empty judgment set.

Effective state is derived and must not be written back as authored follows,
blocks, or other judgments. Removing a source therefore removes state inherited
solely from that source without a compensating publication.

## Publication and compatibility

A published Hydra action creates the canonical addressable Flocking judgment.
Hydra also maintains NIP-02 and NIP-51 mirrors where the library says the
meaning is faithful. Existing standard-list events remain valid fallback input,
but canonical withdrawals and affirmative contrary judgments take precedence.

A local action changes only encrypted persona state. An optional reason remains
private for a local action and becomes the canonical event content for a
published action.

Hydra must replay its existing follow and block records without rewriting
history. Existing public NIP-02/NIP-51 state enters through compatibility
adapters until a person publishes canonical per-target judgments.

## Evaluation point

Hydra computes one effective view before feed lenses sort or render content:

1. Resolve current direct and source judgments.
2. Evaluate block and silence for the author.
3. Evaluate hide and community membership for the object and topic.
4. Exclude removed objects from contextual pins.
5. Return eligibility, certainty, and provenance to the lens and interface.

Indeterminate source state is visible as uncertainty and must not silently
permit content that a complete higher-ranked source might exclude.

## Delivery sequence

### 1. Block vertical slice

- Pin the two Flocking crates and add identity/configuration mappings.
- Persist encrypted block-source grants and source completeness.
- Parse canonical block judgments and NIP-51 fallback lists.
- Publish canonical block, unblock, and withdrawal judgments plus the faithful
  NIP-51 positive-block mirror.
- Support global and community-scoped direct blocks with optional reasons.
- Evaluate direct and inherited blocks before Hydra feed rendering.
- Expose authored versus inherited state, certainty, source event, reason, and
  a concise `Why?` explanation.
- Preserve the existing blocked-author placeholder and explicit reveal as Hydra
  interface policy.

### 2. Remaining ordinary actions

- Add silence and unsilence with first-seen evidence for suspicious backdating.
- Add hide/unhide and community removal/restoration for stable content targets.
- Add contextual pins, aggregation, withdrawal, and local dismissal.
- Apply another person's authored follow state as a non-recursive overlay.

### 3. Discovery and hardening

- Add Reverse Flocking and the direct `Rescue` action.
- Exercise stale, unknown, conflicting, withdrawn, edited, and multi-topic
  cases through deterministic fixtures.
- Complete migration, backup, privacy, security, and protocol documentation.
- Run installed-app verification before declaring Hydra a complete adopter.

## Acceptance criteria for the block slice

- A direct topic block overrides a direct global judgment in that topic.
- A direct judgment overrides every inherited block judgment.
- Ranked source judgments resolve deterministically and retain provenance.
- Unknown higher-ranked source state produces uncertainty rather than an
  invented answer.
- Blocked past and future activity is excluded without deleting stored events.
- Unblock and withdrawal remain distinct and affect only the block faculty.
- Local reasons remain encrypted; published reasons are visibly public.
- Existing public mute lists remain readable as lower-fidelity fallback state.
- Removing a source removes its effective block without changing authored state.
- A clean checkout passes Hydra's complete canonical test gate.
