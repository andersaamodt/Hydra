# Hydra event conventions v1

Status: experimental and release-frozen for Hydra 1.0.

Hydra uses NIP-7D `kind:11` post anchors and NIP-22 `kind:1111` comment anchors. Replies always target immutable anchors.

## Editable object head — kind 30800

An addressable head carries the current body of one post or comment. Its stable `d` tag is `hydra:head:<anchor-event-id>`. It also carries the anchor `e`, anchor `k`, `L=hydra`, `l=object-head`, and `version=hydra-protocol/v1`.

Post heads contain a title and topic tags. Comment heads retain root and immediate-parent topology. Ordinary relays may retain only the newest head; Hydra keeps received versions locally.

## Projection record — kind 30801

An addressable projection record maps one Hydra anchor to one public copy created by that Hydra persona on an external system.

Its stable `d` tag is derived from the anchor, external system, and external object. Public content contains only the external identifier, canonical URL, target community, projection type, public state, and optional current-head reference.

Credentials, retries, payload hashes, errors, and divergence details remain in the encrypted local journal. Receivers verify signatures, anchor ownership, deterministic addresses, and agreement among URL, object type, target, and external identifier.
