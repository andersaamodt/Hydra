# Release readiness

Hydra 1.0 is release-ready only when:

- formatting, linting, Rust tests, UI tests, protocol checks, and extension tests pass;
- a clean desktop package builds on macOS and Linux;
- installed-app automation completes onboarding, posting, editing, replying, Nostr voting, Revisit, Open Nostr, settings, Reddit Bridge guardrails, and export import;
- secrets, app state, and non-release materials are absent from tracked files and history;
- the repository is clean and reproducible from a fresh clone;
- live Reddit OAuth and API smoke tests have passed with the dedicated QA account, or are explicitly recorded as blocked on Reddit credentials.
