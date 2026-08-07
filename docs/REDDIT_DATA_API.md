# Reddit Data API use

This document describes the optional Reddit Bridge in Hydra 1.0. It is the
public implementation and compliance reference for a Reddit Data API access
request.

## Purpose and user benefit

Hydra is a free, noncommercial, open-source, local-first Nostr desktop client.
Its optional Reddit Bridge lets a Redditor use one desktop interface to read a
Reddit community, participate through their own account, and optionally place
a user-authored Hydra discussion on Reddit without surrendering the Hydra
original.

A typical flow is: a user opens `/r/science` in Hydra, reads the current Reddit
listing and one selected thread, manually replies through their linked Reddit
account, and leaves the view. If the user instead writes a Hydra post,
they may explicitly choose to publish a Reddit copy and keep later edits to
that user-owned copy synchronized.

The bridge is not a bot, moderator, scraper, archive, data broker, or model-
training pipeline. It performs no action without the authenticated user's
choice and remains removable without impairing Hydra's Nostr functions.

## Why Devvit is insufficient

Hydra is an installed desktop application, not a subreddit-installed Reddit
app. Its core requires a local encrypted store, operating-system credential
custody, offline operation, a Nostr relay client, and per-user authorization
outside reddit.com. Devvit cannot provide the installed application or its
local Nostr and storage runtime.

## OAuth and API surface

Hydra uses the OAuth installed-app flow with a fixed loopback callback and a
unique, versioned User-Agent. It requests only these scopes:

| Scope | Use |
| --- | --- |
| `identity` | Confirm the Reddit account linked to the active Hydra persona. |
| `read` | Read a user-selected community, rules, thread, or exact object. |
| `history` | Reconcile only the linked user's own Reddit projections. |
| `submit` | Submit a post or reply the user explicitly chose to crosspost. |
| `edit` | Edit or delete only the linked user's own Reddit content. |

The implemented endpoints are:

- `GET /api/v1/me`;
- `GET /r/{subreddit}/{hot|new|top|controversial}`;
- `GET /r/{subreddit}/about/rules`;
- `GET /comments/{post_id}`;
- `GET /api/info` for one known fullname;
- `GET /user/{linked_username}/overview` for projection reconciliation;
- `POST /api/submit` and `POST /api/comment`;
- `POST /api/editusertext` and `POST /api/del`.

Hydra requests at most 100 listing items per page, bounds responses to 8 MiB,
and rejects oversized or excessively deep threads. It does not continuously
poll all of Reddit. Reads occur for the active view, an explicit refresh, or
reconciliation of a recently active user-owned projection. A rate-limit
response stops the operation and is surfaced to the user; Hydra does not evade
or parallelize around Reddit limits.

## Data flow and retention

API-fetched third-party Reddit bodies exist only in the active application
session. They are not written to Hydra's durable event store, logs, backups, or
Nostr events and are not transmitted to a Hydra server, another Hydra user, an
AI service, or any other third party.

Hydra durably stores only the minimum local metadata needed for a user-requested
Reddit projection: the Reddit fullname and URL, parent relationship, timestamp,
payload hash, current status, and synchronization state. This metadata is not
published to Nostr as a copy of Reddit content.

Because the user deliberately created the Reddit copy from a public Hydra
object, Hydra also publishes a narrow Nostr projection record mapping those two
public manifestations. It contains the Reddit object identifier, canonical
URL, target subreddit, projection type, public state, and Hydra object
reference. It contains no API-fetched Reddit body, credential, local payload
hash, retry detail, error, or divergent Reddit edit.

Reddit OAuth credentials are persona-bound and held in the operating-system
credential vault or Hydra's encrypted, permission-restricted local fallback.
They never enter Nostr events, application logs, Hydra backups, or the browser
companion. Disconnecting the account deletes the local Reddit credential.

The official Reddit account-data export importer is independent of the Data
API. It processes a file the user obtained from Reddit, locally reads only
posts and comments selected by that user, and never uploads the archive.

## Deletion and removals

Hydra does not use API-derived copies to preserve or redisplay deleted or
removed third-party Reddit bodies. The current Reddit view is refreshed from
Reddit and displays Reddit's current state.

Hydra may edit a Reddit copy only when the same user first authored the item in
Hydra and explicitly projected it to Reddit. An optional user-directed edit may
append a link to the Hydra original or replace that user-owned copy with a
notice linking to the original. These operations cannot archive, replace, or
withdraw a Reddit-originated item or an API-fetched parent or sibling body.

## Automated activity

Hydra does not operate an app account and does not post unsolicited content.
Each Reddit write is attributed to the Reddit account that authorized it.
Posting and commenting are user-directed. Hydra 1.0 does not request Reddit's
`vote` OAuth scope or project votes to Reddit; Hydra's Nostr reactions and
reaffirmations remain independent of Reddit.

## Security and compliance

Hydra does not sell Reddit data, monetize the bridge, train models with Reddit
data, profile Redditors, send private messages, or perform moderation actions.
It does not mask its OAuth identity or User-Agent. The browser companion has no
Reddit credential or Nostr signing key.

See the project [`PRIVACY.md`](../PRIVACY.md),
[`docs/SECURITY.md`](SECURITY.md), and the implementation in
[`crates/hydra-reddit`](../crates/hydra-reddit/).
