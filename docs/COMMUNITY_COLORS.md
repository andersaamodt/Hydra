# Community colors

Hydra treats a community image and a community color scheme as independent appearance decisions. Both use the persona's explicitly selected appearance sources, but support for one never changes the result of the other.

## Scheme

One scheme contains four canonical opaque sRGB colors:

- `light-base` supplies the identity of light-mode surfaces.
- `light-accent` supplies the identity of light-mode actions and selection.
- `dark-base` supplies the identity of dark-mode surfaces.
- `dark-accent` supplies the identity of dark-mode actions and selection.

Each color is lowercase `#rrggbb`. Hydra derives component surfaces, foreground text, disabled states, and semantic status colors. Published choices cannot provide CSS, alpha, fonts, layout, text colors, or executable resources.

## Nostr mapping

A public choice is an addressable kind `30802` event with an empty content field and these tags:

```text
["d", <bare-topic>]
["v", "1"]
["t", <bare-topic>]
["j", "set"]
["color", "light-base", <color>]
["color", "light-accent", <color>]
["color", "dark-base", <color>]
["color", "dark-accent", <color>]
```

A withdrawal uses `["j", "withdraw"]` and contains no `color` tags. The signer, topic address, version, action, exact tag cardinality, and colors are validated before materialization.

## Evaluation

For one persona and topic, Hydra selects each author's newest valid replaceable choice. A non-empty direct choice by the viewing persona wins. Otherwise Hydra groups the exact four-color tuples chosen by selected appearance sources and picks the tuple with the most distinct support, then newest support, then canonical tuple order as a deterministic tie break.

Incomplete selected sources remain explicit in evaluator input and never become silent negative evidence. The runtime exposes direct/followed provenance without writing the effective result back as an authored choice.

## Rendering

The current route selects the effective community scheme before rendering. Community schemes affect the main window only while a community is the active context; multi-community views and the detached Settings window use Hydra's base scheme. The user's Light, Dark, or System mode and global `Use community colors` preference remain authoritative.

Hydra maps `base` to application surfaces through `--surface-seed` and maps `accent` to interactive roles through `--accent-seed`. The palette compiler normalizes unsafe accents until primary-control text reaches a WCAG contrast ratio of at least 4.5:1. Authored colors never replace Hydra's foreground or semantic colors.
