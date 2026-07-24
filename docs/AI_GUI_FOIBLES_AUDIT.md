# Anti-AI-GUI-foibles audit

This is derivative evidence for `docs/AUTHORITATIVE_SPEC.md`. Hydra was audited on 2026-07-23 against Phronesis `standards/gui/anti-ai-gui-foibles.md` at revision `28af77100a6f1c3cda002e80429ba12bd7f0fce4`.

## Removed

- The permanent slogan beneath the app name.
- View kickers, subtitles, and other doubled title treatments.
- Permanent context-panel explanations of Hydra's purpose and philosophy.
- Decorative marks and boxed presentation in empty states.
- Radial and button gradients, backdrop blur, and ornamental glow.
- One-sided active-navigation and comment-selection rails.
- Rounded pills used for ordinary filters, provenance, communities, and state.
- Floating card treatment around every feed entry.
- Nested card treatment around settings and supporting information.
- Repeated Open Nostr explanatory copy on every result.
- Verbose welcome, messaging, Reddit, and settings introductions.

## Retained deliberately

- Full focus outlines remain because keyboard focus must be unambiguous.
- Form help remains where a setting has non-obvious privacy, interoperability, or irreversible consequences.
- Modal descriptions remain when the transient decision needs consequences or evidence explained at decision time.
- Toasts remain for asynchronous operations whose completion is otherwise invisible.
- The right-side status rail remains because relay, identity, and replication readiness are live operational state rather than product marketing.
- Borders remain where they express layout structure, such as the boundary between the context rail and the main discussion.
- Dialog shadow remains as a single separation cue for a temporary layer.

## Regression checks

The UI tests reject permanent title/subtitle structures, explainer slogans, mascot empty states, gradients, backdrop blur, pill radii, and one-sided selection rails. Hands-on desktop checks cover the main feed, Open Nostr, `/h/` and `/r/` chambers, Reddit Bridge, settings, messaging, dialogs, keyboard focus, dark mode, and light mode.
