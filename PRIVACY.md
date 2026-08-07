# Privacy policy

Effective August 7, 2026.

Hydra is a local-first desktop Nostr client. Its optional Reddit Bridge lets a
person connect their own Reddit account and perform actions they choose. Hydra
does not operate a hosted social service, analytics service, advertising
service, or central user database.

## Data Hydra handles

Hydra may handle the following data on the user's device:

- Nostr persona keys, profiles, events, relay choices, messages, follows,
  blocks, drafts, settings, and local memory features;
- a connected Reddit account's OAuth tokens, username, and account identifier;
- Reddit posts, comments, community rules, and account history requested for
  the active Reddit view;
- identifiers, URLs, timestamps, payload hashes, and synchronization state for
  Reddit copies the user chose to create from Hydra; and
- posts and comments the user selects from their own official Reddit
  account-data export.

Hydra does not collect telemetry. It does not sell personal data, serve ads,
profile users for advertising, or train or fine-tune machine-learning models
with Reddit data.

## Where data goes

Reddit API requests travel directly between the user's device and Reddit.
Reddit OAuth credentials remain on the device and are not sent to Nostr
relays, the Firefox companion, Hydra backups, analytics services, or the Hydra
developer.

Third-party Reddit post and comment bodies requested for live browsing are
held only in the active application session. Hydra does not write those bodies
to its durable event store, publish them to Nostr, include them in backups, or
send them to another Hydra user or third party.

When a user deliberately creates a Reddit copy of their own Hydra post or
comment, Hydra publishes a Nostr projection record that identifies that copy.
The record contains the Reddit object identifier, canonical URL, target
subreddit, object type, public state, and Hydra object reference. It contains no
API-fetched Reddit body, credential, payload hash, retry detail, error, or
divergent Reddit edit.

Hydra publishes to Nostr only when the user performs an action that clearly
publishes a Nostr event. Public Nostr events are public and may be retained by
independent relays and other clients outside Hydra's control. Private local
records remain local or encrypted unless the user exports them.

Official Reddit account-data exports are supplied by the user and processed
locally. Hydra reads only the selected user's posts and comments. Imported
writing remains local unless the user separately chooses to publish it to
Nostr.

## Retention and deletion

Live Reddit bodies are not durably retained and leave the active view when it
is replaced or the application exits. Reddit projection metadata is retained
locally only while needed to maintain the Reddit copies the user requested.

Reddit OAuth credentials remain until the user disconnects the Reddit account,
revokes Hydra through Reddit, or deletes Hydra's local data. Disconnecting
Reddit deletes Hydra's locally stored Reddit credential for that persona.

The user controls Hydra's local application data. It is stored under
`~/hydra` by default, or at the location selected through `HYDRA_HOME`, and can
be deleted by removing that directory after closing Hydra. Deleting local data
does not retract public Nostr events already accepted by independent relays.

Hydra refreshes Reddit content from Reddit rather than using a permanent copy.
Content Reddit reports as deleted, removed, or unavailable is not reconstructed
from API-derived bodies in the Reddit view.

## Security

Persona keys use the operating-system credential vault or an encrypted,
permission-restricted local fallback. Reddit credentials are persona-bound.
Hydra validates OAuth callback state and loopback destinations, bounds Reddit
response sizes and thread depth, rejects malformed identifiers and URLs, and
treats network content as hostile input. Additional controls are documented in
[`docs/SECURITY.md`](docs/SECURITY.md).

## User choices

Reddit integration is optional. Users may use Hydra without linking a Reddit
account. Reddit posting, commenting, editing, and deleting occur only after a
user chooses the corresponding action or explicitly enables a visible
projection setting. Hydra 1.0 does not request Reddit's `vote` OAuth scope or
project votes to Reddit; Nostr voting remains independent.

Questions about this policy may be opened in the
[project's public issue tracker](https://github.com/andersaamodt/Hydra/issues)
without including private keys, OAuth tokens, private messages, or other
sensitive information. Hydra operates no service-side user store, so the
developer ordinarily has no copy of local application data to retrieve or
delete.

Hydra may update this policy when its behavior or legal obligations change.
Material changes will be recorded in the public repository.
