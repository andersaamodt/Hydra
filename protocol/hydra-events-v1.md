# Hydra event conventions v1

Status: experimental and release-frozen for Hydra 1.0.

Hydra uses NIP-7D `kind:11` post anchors and NIP-22 `kind:1111` comment anchors. Replies always target immutable anchors.

## Persona user flair — kind 0 extension

A persona may include one optional `flair` string in its standard replaceable NIP-01 profile JSON. Hydra treats it as a self-authored, global, non-expiring profile value; clearing it omits the field. It is limited to 32 Unicode scalar values and cannot contain control or bidi-control characters. Hydra preserves unrelated profile fields whenever it republishes metadata.

## Editable object head — kind 30800

An addressable head carries the current body of one post or comment. Its stable `d` tag is `hydra:head:<anchor-event-id>`. It also carries the anchor `e`, anchor `k`, `L=hydra`, `l=object-head`, and `version=hydra-protocol/v1`.

Post heads contain a title and topic tags. Comment heads retain root and immediate-parent topology. Ordinary relays may retain only the newest head; Hydra keeps received versions locally.

## Projection record — kind 30801

An addressable projection record maps one Hydra anchor to one public copy created by that Hydra persona on an external system.

Its stable `d` tag is derived from the anchor, external system, and external object. Public content contains only the external identifier, canonical URL, target community, projection type, public state, and optional current-head reference.

Credentials, retries, payload hashes, errors, and divergence details remain in the encrypted local journal. Receivers verify signatures, anchor ownership, deterministic addresses, and agreement among URL, object type, target, and external identifier.

## Post flair choice — kind 30803

An addressable post-flair event records one person's current label choice for one immutable `kind:11` post anchor. Its deterministic `d` tag is `hydra:post-flair:<sha256(anchor|scope)>`, where scope is `all` for the default choice or the lowercase community key for an override.

Every event carries exactly one `e`, `k=11`, `version=hydra-protocol/v1`, and `status` tag. A set choice has `status=set` and exactly one bounded `flair` tag. A withdrawal has `status=withdraw` and no `flair` tag. A community override also carries exactly one matching `t` tag; the default omits `t`.

Clients select the current event by the normal addressable-event rules. They derive the displayed flair locally from distinct current signers; the winning display value is not persisted on the post itself.
