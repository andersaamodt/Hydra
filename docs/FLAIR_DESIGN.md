# Post labels and global user flair

Status: proposed for post-1.0 work. Hydra 1.0 remains release-frozen.

## Decision

Hydra should have two deliberately different features:

- **Global user flair** is one optional, public label a persona chooses for
  themself. It travels with that persona everywhere in Hydra.
- **Post flair** is the leading visible label derived from signed label choices
  made by people about a post. It can resolve differently in each `/h/`
  community where the post appears.

There is no flair on a subreddit, Hydra community, or hydrant itself. A
community only provides context for resolving a post's labels.

Hydra should not support third-party or moderator-assigned user flair now. That
would be a public branding/credential system, with materially different abuse
and trust semantics. Awarded claims belong to a separate future credential or
attestation feature.

## What Hydra has today

Hydra currently models neither feature.

The existing `ObjectHead.communities` values say that a post appears in
`/h/science`, for example. Provenance and durability chips describe Hydra
state, and persona display names are global identity metadata. None of those
are flair.

## Hydrants, topics, tags, and flair

Hydrants are communities or topics. They are not a user-facing tagging feature
and do not have flair of their own.

Under the hood, Nostr represents the topic names attached to a post with `t`
tags. That is a storage/protocol detail and Hydra should not call them tags in
the interface or ordinary product documentation.

Post flair is separate in the product model. Each person gets one current flair
choice for a post, rather than an open-ended bag of tags. Across people, those
choices form a small set of candidate labels such as `Question` or
`Field report`.

The interface renders the leading post label as a compact flair chip. The label
detail surface shows the other proposed labels and their support. This keeps
`flair` as a useful visual treatment without inventing a separate semantic
object or encouraging people to attach many tags to every post.

User-facing copy may say `flair`; internal types and protocol documentation
should say `post label` or `label choice`.

## Global user flair

### Behavior

- A persona has zero or one current user flair.
- The persona chooses and can replace or clear it.
- The same flair appears beside the persona wherever sufficient space exists:
  post bylines, comment bylines, messages, search results, and the profile.
- Compact or safety-sensitive surfaces may omit it, but never substitute a
  different flair.
- The detail surface says `Chosen by this persona`.
- It grants no permissions and carries no verification treatment.

Examples include `Hydra tinkerer`, `Ask me about fungi`, and `Recovering
academic`.

### Validation and appearance

- Plain text only: no HTML, URLs, images, custom CSS, or remote fonts.
- 1-32 Unicode scalar values after trimming.
- One line and compliant with Hydra's existing unsafe-inline-text checks.
- Text is always present; color is decorative and never conveys meaning.
- Hydra assigns a stable tone from its bounded accessible theme palette.
  Publishers cannot choose foreground or background colors.
- Malformed flair is ignored without hiding an otherwise valid profile.

### Profile representation

User flair is simply part of the persona's replaceable Nostr profile record
(`kind:0`), alongside its name and other public profile fields:

```json
{
  "name": "alice",
  "display_name": "Alice",
  "flair": "Hydra tinkerer"
}
```

Updating the display name or user flair republishes the complete current
profile. Clearing user flair omits `flair` from the next profile record. Clients
that do not know the field can safely ignore it.

`flair`, not `hydra_flair`, is the right field name. The meaning is generic and
useful to any social client: one short, self-authored descriptor displayed next
to a person's name. Prefixing it with an application name would falsely define
it as Hydra-private data and discourage interoperability.

The field is an experimental profile extension, not an existing Nostr standard.
Hydra should document its exact meaning and seek addition to NIP-24 if other
clients adopt it. It must also preserve every unknown field from the persona's
current profile whenever it republishes `kind:0`; editing flair must never erase
profile data written by another client.

Hydra currently has local personas but no durable remote profile projection.
Implementation needs a bounded `PersonaProfile` keyed by public key, containing
display metadata, optional user flair, and the winning profile event timestamp
and id. Signing custody remains in the local `Persona` type and must not leak
into the public projection.

### Why the profile field is preferable to the nearby NIPs

- **NIP-32 labels** classify events, people, relays, URLs, or topics using a
  repeatable vocabulary. Its own guidance says unique values are values rather
  than labels. Free-form personal flair is a value in a profile, and a
  third-party label targeting a person has the assignment semantics Hydra
  explicitly does not want.
- **NIP-38 status** is designed for live, optionally expiring statements such
  as `Working` or currently playing music. It may be displayed beside a name,
  but using status storage for stable identity decoration gives the value the
  wrong lifecycle and meaning.
- **NIP-58 badges** are defined and awarded by an issuer, then accepted for
  profile display by the recipient. That is the third-party credential model,
  not self-chosen flair.
- **NIP-78 app data** is explicitly for application-specific data that does not
  need interoperability. Global flair is intended to interoperate, so hiding it
  in Hydra application data would be the wrong tradeoff.

The profile field therefore has the correct subject, author, lifecycle, fetch
path, and deletion behavior with the least semantic distortion.

## Communal post labels

### One current choice per person

For each post, a persona may publish:

- one **default** label choice that applies everywhere the post appears; and
- an optional override for any particular `/h/` community.

For one persona in one community, the community override replaces that
persona's default. It does not count as a second endorsement.

This gives the desired default behavior without inventing a primary community:

```text
Alice's default choice:              Question
Alice's /h/science override:         Field report

Effective in /h/gardening:           Question
Effective in /h/science:             Field report
Effective in a newly added hydrant:  Question
```

The post author uses this same mechanism. The composer makes one initial
default choice easy and keeps per-community overrides under an advanced
control. Other people can later choose or change their own label.

### Resolution

For each `(post, community)`:

1. Resolve each persona to at most one current effective choice: their
   community override, otherwise their default.
2. Exclude invalid events and choices from personas hidden by the viewer's
   ordinary local block/filter rules.
3. Count distinct personas supporting each canonical label.
4. If one label has the highest count, it is the leading label and is rendered
   as post flair.
5. If the top labels tie and the post author's effective choice is among them,
   the author's choice leads.
6. Otherwise render a neutral `N labels` chip rather than choosing an arbitrary
   winner.

Matching should use a canonical comparison form. At minimum, trim and
Unicode-normalize before case-insensitive comparison so `Question` and
`question` do not split support. Display the post author's spelling when they
support the winning canonical label; otherwise choose the lexicographically
smallest valid spelling among its current supporters. Relay arrival order must
not affect the rendered result.

Counts mean `signed choices currently available to this client`, not global
truth. Relay coverage and local filtering can make the leading label differ
between viewers. The interface should say `7 people label this Question`, not
`The community says Question`.

Do not add reputation weighting, moderator weighting, or a new trust graph in
the first version. One public key contributes at most one current choice per
post/community. If label brigading becomes a real problem, explicit source
selection can be added later using Hydra's existing design language.

### Alternatives and abuse controls

The leading chip exposes a hover, keyboard-focus, and click/tap popover:

```text
Labels in /h/science

Question       7 people   <- leading
Field report   4 people
Discussion     2 people

[Choose a label]  [Hide a label from my view]
```

- Alternatives remain collapsed by default so a single hostile proposal does
  not become ambient branding.
- Blocked or locally filtered personas do not contribute choices or spellings.
- A persona can locally hide a particular label value without hiding the post.
- Label text is inert and follows the same validation as user flair.
- Post authors have one ordinary choice and the tie-break described above, not
  a global veto over what other clients may derive.

### Nostr representation

Hydra needs one current, replaceable choice rather than a history of permanent
tags. It should define one addressable label-choice event. `kind:30802` is the
proposed allocation, subject to the protocol review performed when the feature
is implemented.

An active community-scoped choice has this shape:

```json
{
  "kind": 30802,
  "tags": [
    ["d", "hydra:post-label:<anchor-id>:science"],
    ["e", "<anchor-id>"],
    ["k", "11"],
    ["t", "science"],
    ["label", "Field report"],
    ["version", "hydra-protocol/v2"],
    ["status", "active"]
  ],
  "content": ""
}
```

A default choice uses `all` instead of a community in its `d` tag and omits
`t`. A withdrawal republishes the same address with `status=withdrawn` and no
`label` tag. The normal addressable-event winner rules select one current event
per publisher, post, and scope.

The event targets the immutable post anchor, not the editable object head, so
label choices survive post edits. A community-scoped choice is only effective
when the current post head actually contains that community.

This is classification, not exclusion, membership, pinning, or authority. It
should not be forced into Flocking's judgment faculties, though it can reuse
similar storage, current-event, completeness, and provenance patterns.

The materialized domain data is independent of `ObjectHead`:

```text
PostLabelChoice {
    author: NostrPublicKey,
    target: AnchorId,
    scope: All | Community(CommunityKey),
    value: Option<FlairText>,
    changed_at: u64,
    event_id: String,
}
```

The runtime derives per-community leaders and alternative counts. It does not
persist one supposedly canonical winning flair on the post.

## Interface design

### Community feed

```text
[Question]  Why are my tomato leaves curling?                 3h
            Alice [Hydra tinkerer]                     /h/gardening
            12 replies  Save  React  Label  Hide
```

- The leading post label precedes the title.
- Global user flair follows the resolved display name.
- `Label` opens the current choices and lets the active persona select an
  existing value or propose a new one.
- Clicking the leading post flair opens its detail popover; an explicit action
  there can filter the feed by that label.

### Discussion and comments

- The discussion header shows the resolved post flair for the active community.
- Post and comment bylines show the author's global user flair.
- The label detail surface shows exact counts and the viewer's current choice.
- Blocked placeholders and compact notifications omit flair when provenance or
  space would be ambiguous.

### Aggregated and direct-link contexts

Hydra must not invent a primary community:

- In `/h/<community>`, resolve and show that community's leading label.
- In an aggregated feed, show the label only if every attached community
  currently resolves to the same value. Otherwise show `N flairs` with
  `/h/community · Flair` rows.
- On a direct discussion link with no community context, use the same all-topic
  summary and let the viewer select a community context.
- Global user flair remains the same in every context.

### Post composer

The composer provides one optional `Post flair` field. Its initial value becomes
the author's default choice everywhere the post appears:

```text
Hydrants       gardening, science
Post flair     [ Question       v]
               [Customize by hydrant]
```

Expanding the control allows per-community overrides. Publishing the post and
its initial label choices is one user action but produces independent queued
events, so a label publication failure never loses or mutates the post.

### Changing a post label

The label picker shows existing candidates as a radio list with support counts,
plus `Propose another label`. The active scope is explicit:

- `Everywhere this post appears` updates the persona's default choice.
- `Only /h/science` updates the persona's community override.
- `Use my default here` withdraws the community override.
- `Remove my choice` withdraws both only after confirmation if both are in
  scope.

### User-flair editor and profile

Persona settings and the persona's own profile expose `Set my user flair`. The
editor previews the chip and explains:

> This public, self-chosen label travels with this persona throughout Hydra. It
> is not a verified credential or a role assigned by other people.

Profiles display that one flair beside the name. There is no community-flair
section and no third-party assignment control.

## Reddit bridge

Reddit post and user flair are controlled by subreddit templates and moderator
permissions. They are not equivalent to Hydra's labels or self-chosen global
user flair.

Initial behavior:

- Do not automatically import Reddit flair into Hydra labels or user flair.
- Do not automatically project Hydra flair to Reddit.
- Keep any Reddit flair required for an explicit projection in adapter-owned
  metadata.

A later projection flow may let the user explicitly map a leading Hydra post
label to an available subreddit template. Mapping failure must never fail or
mutate the Hydra original.

## Delivery plan

1. Add global user flair to profile publication, bounded remote profile
   ingestion, runtime views, and bylines.
2. Add `FlairText`, `PostLabelChoice`, addressable event parsing/publication,
   current-choice storage, and withdrawals.
3. Add per-community resolution with unique-person counts, tie handling, local
   filters, and completeness/provenance details.
4. Add initial/default post flair to the composer and label controls to feeds
   and discussions.
5. Add alternative popovers, exact-label filtering, accessibility coverage,
   and protocol/UI tests.

Global user flair is the smaller feature and should ship first. Communal post
labels should follow as a separate increment because they add aggregation and
abuse-handling semantics rather than a field on `ObjectHead`.

## Acceptance criteria

- User flair is self-chosen, global, replaceable, and clearable.
- No user can assign user flair to another persona.
- A persona contributes at most one effective label to a post in a community.
- Default post-label choices apply everywhere unless that persona overrides a
  particular community.
- The unique leading label is visible; alternatives and their support are
  inspectable without being ambient.
- Ties never choose an arbitrary primary label.
- Counts are described as locally available signed choices, not global truth.
- Malformed or hostile label data cannot suppress content or inject styles.
- Existing posts and profiles without flair render unchanged.

## References

- [NIP-01: Nostr events and user profiles](https://github.com/nostr-protocol/nips/blob/master/01.md)
- [NIP-24: extra profile metadata fields](https://github.com/nostr-protocol/nips/blob/master/24.md)
- [NIP-32: labeling](https://github.com/nostr-protocol/nips/blob/master/32.md)
- [NIP-38: user statuses](https://github.com/nostr-protocol/nips/blob/master/38.md)
- [NIP-58: badges](https://github.com/nostr-protocol/nips/blob/master/58.md)
- [NIP-78: arbitrary custom app data](https://github.com/nostr-protocol/nips/blob/master/78.md)
