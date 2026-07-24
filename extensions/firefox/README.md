# Hydra Firefox companion

Firefox companion for opening Reddit objects in Hydra, compacting Hydra record markers, and native-app messaging.

The extension never receives or stores Nostr private keys, Reddit OAuth tokens, archive data, comment bodies, or remote code. It hands the current Reddit URL to the installed Hydra desktop native host only after an explicit user action.

Load `manifest.json` temporarily from `about:debugging` during development. Release packaging installs `libexec/hydra-native-host` and the matching `org.hydra.desktop` native-messaging manifest; `install-native-host` performs the user-local Firefox registration for unpackaged builds.
