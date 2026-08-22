# Post and user flair design

Status: proposed for post-1.0 work. Hydra 1.0 remains release-frozen.

## Decision

Hydra should support both post flair and user flair as optional, public,
topic-scoped self-expression:

- **Post flair** is chosen by the post author. A post may have at most one flair
  for each `/h/` topic it belongs to.
- **User flair** is chosen by the persona for themself. A persona may have at
  most one flair for each `/h/` topic.

Flair is descriptive, not authoritative. It does not grant permissions, prove a
credential, rank a person, or imply approval by a community. Hydra communities
are ownerless, so there is no canonical moderator-owned flair catalog.

Examples:

- A post shared to `/h/gardening` and `/h/science` can be `Question` in the
  former and `Field report` in the latter.
- The same persona can be `Tomato enthusiast` in `/h/gardening` and
  `Ecologist` in `/h/science`.

## What Hydra has today

Hydra does not currently model post or user flair.

The nearby features are not flair:

- `ObjectHead.communities` are membership/topic tags, not content
  classification within a topic.
- Provenance and durability chips describe Hydra state.
- Persona display names are global to the persona rather than topic-scoped.
- Norms, pins, removals, and other signed judgments can shape a viewer's
  experience, but do not decorate an author or post.
- NIP-58 badges are issuer-awarded credentials or recognition. They should not
  be presented as self-chosen user flair.

## Product model

### Shared rules

- Flair is optional.
- Flair is plain text: no HTML, URLs, images, custom CSS, or remote fonts.
- A flair contains 1-32 Unicode scalar values after trimming, is one line, and
  passes Hydra's existing unsafe-inline-text checks.
- A colon is allowed because the topic is carried by the label namespace, not
  encoded into the visible text.
- A malformed or unsupported flair is ignored without hiding an otherwise
  valid post or profile.
- Text is always rendered; color is supplementary and never conveys meaning.
- The client assigns a stable accessible tone from its bounded theme palette.
  Publishers do not control foreground or background colors.

### No canonical catalog

The first version should not add flair definitions, templates, or a special
flair authority. The composer offers free text plus recently observed values in
the current topic. Suggestions are conveniences, not an allowlist.

This keeps the protocol small and avoids turning whoever publishes a catalog
into a de facto community owner. A later version may let a persona follow
another persona's flair vocabulary, using the same explicit source-selection
pattern as Hydra's other community-shaping choices.

### Credentials stay separate

Statements such as `Verified physician`, `Maintainer`, or `Moderator` can be
misleading when self-applied. Hydra should render self-chosen flair as
`Chosen by this persona` in its detail surface and reserve verified or awarded
claims for NIP-58 badges or a future attestation design. A badge may appear on a
profile, but it must not silently become user flair.

## Nostr representation

Use NIP-32 `L` and `l` tags for portable self-labeling. The reverse-domain
namespaces are open conventions, not an assertion that Hydra owns a topic.

### Post flair

Post flair lives on the existing replaceable Hydra object head (`kind:30800`).
For a post in `/h/science` with the flair `Question`, its head includes:

```json
["L", "org.hydra.flair.post.science"]
["l", "Question", "org.hydra.flair.post.science"]
```

Rules:

- The suffix is a valid normalized `CommunityKey`.
- The community must also appear in the head's topic tags.
- There is at most one `l` value for each post-flair namespace.
- Comment and norm heads ignore post-flair namespaces.
- An edit republishes the complete current head, so changing or clearing flair
  follows the existing object-head lifecycle.

The domain representation is a map rather than a single field:

```text
ObjectHead.flairs: BTreeMap<CommunityKey, FlairText>
```

This is necessary because one Hydra post can belong to several ownerless topic
communities and has no primary community.

### User flair

User flair lives on the persona's replaceable NIP-01 profile metadata event
(`kind:0`). For the same topic it includes:

```json
["L", "org.hydra.flair.user.science"]
["l", "Ecologist", "org.hydra.flair.user.science"]
```

Rules:

- There is at most one value for each user-flair namespace.
- The profile event author is the flair subject; Hydra does not accept a
  third-party user-flair assignment as that person's self-chosen flair.
- Updating a display name or any user flair republishes the complete current
  profile metadata, preserving all other known profile fields and flair tags.
- Clearing a flair removes both of its namespace tags from the next profile
  event.

Hydra currently models local personas but not a durable remote profile view.
Implementation therefore needs a bounded `PersonaProfile` projection keyed by
public key, containing display metadata, `flairs`, and the winning profile
event timestamp/id. Local signing custody remains in `Persona` and must not be
mixed into this public projection.

### Why not `kind:1985` for current user flair?

NIP-32 label events are immutable. Deletion and replacement can be observed
inconsistently across relays, which is a poor fit for a single current cosmetic
choice. A `kind:0` profile is already replaceable and represents the persona's
current public self-description. Third-party `kind:1985` labels can still be
retained as evidence, but they are not self-chosen user flair.

## Interface design

### Community feed

```text
[Question]  Why are my tomato leaves curling?                 3h
            Alice [Tomato enthusiast]                 /h/gardening
            12 replies  Save  React  Hide
```

- Post flair precedes the title and is visually stronger than user flair.
- The byline uses the resolved display name rather than a raw public key when
  known; user flair follows the name.
- Clicking post flair applies a local exact-match feed filter. The filter bar
  makes the active filter obvious and removable.
- Clicking user flair opens the persona profile, not a reputation view.
- A tooltip or detail popover says who chose the flair and its `/h/` scope.

### Discussion and comments

- The discussion header shows the post flair beside the title.
- Author bylines and comment bylines show user flair for the active topic.
- Flair does not repeat inside blocked placeholders or notifications where
  space and provenance are ambiguous.

### Aggregated and direct-link contexts

Hydra must not invent a primary community:

- On a `/h/<topic>` route, show only that topic's post and user flair.
- On an aggregated feed, show a post flair only when the post has exactly one.
  If it has several, show a quiet `N flairs` control that lists
  `/h/topic · Flair` pairs.
- User flair is hidden in aggregated feeds because there is no active topic.
- On a direct discussion link with no route context, list all post flairs with
  their topic names and omit user flair until a topic context is selected.

### Composing a post

After topic parsing, the post composer shows one optional row per topic:

```text
Topics       gardening, science
gardening    [ Question       v]
science      [ Field report   v]
```

Each field accepts free text and suggests the persona's recent choices followed
by recently observed values. Removing a topic removes its draft flair after a
clear inline warning. Drafts store the map locally.

### Setting user flair

The community header and the persona's own profile both expose `Set my flair in
/h/<topic>`. The modal contains one optional text field, a live chip preview,
and this explanation:

> This is your public, self-chosen label in /h/topic. It is not a verified
> credential or a community role.

Saving republishes the persona's complete profile metadata. The action must show
the ordinary public-publication affordance; it is never silently local-only.

### Profile

The profile shows the active topic's user flair near the display name. An
expanded `Community flair` section lists the persona's other topic/value pairs.
For the active persona, each row is editable. For anyone else, details say
`Chosen by <display name>`.

## Reddit bridge

Reddit post and user flair are controlled by subreddit-specific templates and
permissions, so the bridge must not pretend they are equivalent to Hydra's
ownerless flair.

Initial behavior:

- Do not automatically copy Reddit flair into Hydra flair.
- Do not automatically project Hydra flair to Reddit.
- Preserve any Reddit flair needed for an explicit projection only in
  adapter-owned metadata.

A later bridge feature may let the user explicitly map one Hydra topic flair to
one currently available subreddit template at projection time. Failure to map
or apply Reddit flair must never fail or mutate the Hydra original.

## Delivery plan

1. Add `FlairText` validation and topic-to-flair maps to post heads, drafts, and
   the public profile projection.
2. Parse and publish the NIP-32 tags on object heads and profile metadata;
   begin fetching and materializing bounded `kind:0` profile events.
3. Carry effective flairs through the runtime snapshot without adding ranking
   or trust semantics.
4. Add composer/profile controls, community-context rendering, and accessible
   chip styles.
5. Add exact-match post-flair filtering and protocol/UI tests.

Post flair should ship before user flair if the work is split. It uses Hydra's
existing object-head sync path and validates the multi-community interaction.
User flair then adds the more substantial remote-profile ingestion work.

## Acceptance criteria

- A post can independently set, edit, and clear one flair per attached topic.
- A persona can independently set, edit, and clear one public flair per topic.
- Community views show only the current topic's flair.
- Aggregated views never choose a primary topic implicitly.
- Self-chosen flair is never rendered as verified, awarded, or permissioned.
- Malformed remote flair cannot suppress valid content or inject styles.
- Flair is readable and distinguishable without color and under all Hydra
  themes.
- Existing posts and profiles without flair render unchanged.

## References

- [NIP-32: Labeling](https://github.com/nostr-protocol/nips/blob/master/32.md)
- [NIP-58: Badges](https://github.com/nostr-protocol/nips/blob/master/58.md)
