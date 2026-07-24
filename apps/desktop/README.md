# Desktop application

Hydra has one canonical Tauri 2 desktop shell for macOS, Linux, and Windows. It embeds the framework-free `hydra-ui` HTML/CSS/JavaScript interface and transports typed requests to the Rust `hydra-runtime` sidecar.

The runtime's action dispatcher is the sole authority for action names, input validation, and workflow policy. The shell owns no second allowlist, domain state, Nostr semantics, Reddit behavior, credentials, or archive policy.

User data is always stored outside the repository under the platform data root (or `HYDRA_HOME` for isolated testing).
