# Hydra foreign-community bridge protocol v1

Hydra integrates optional foreign community systems through a local executable. Hydra core does not link adapter SDKs, own adapter credentials, interpret provider identifiers, or implement provider rate-limit and cache policy.

The executable reads one JSON request from standard input and writes one JSON response to standard output. Both are limited to 2 MiB. The host clears the child environment, supplies only its managed adapter data directory, and correlates every response with an unpredictable request ID.

Requests contain `protocol`, `requestId`, `operation`, optional `personaId`, and `payload`. Responses contain the same `protocol` and `requestId`, `ok`, and exactly one of `result` or `error`. Version 1 is `hydra-foreign-community-bridge/v1`.

Every adapter implements `describe`. Its result contains a stable lowercase `id`, display `name`, semantic `version`, exact `protocol`, capability tokens, and `credentialCustody: "bridge"`. Hydra rejects an install whose descriptor does not match the requested adapter or claims host-owned credentials.

Operation payloads are capability-specific but provider-neutral at the host boundary. The initial vocabulary includes `identity`, `oauth.begin`, `oauth.complete`, `oauth.unlink`, `community.browse`, `community.rules`, `thread.fetch`, `object.fetch`, `post.create`, `comment.create`, `object.edit`, `object.delete`, and `export.preview`. Unsupported operations fail explicitly.

Hydra installs a verified local executable into its user data directory, probes `describe`, then atomically records the executable path, SHA-256 digest, and descriptor. Adapter application state and credentials live outside both source repositories.
