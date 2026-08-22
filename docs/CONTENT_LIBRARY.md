# Hydra content library

Hydra retains public Hydra-native and Nostr content in an unencrypted,
human-readable library under `HYDRA_HOME/library` (normally `~/hydra/library`).
The library is the canonical local copy of public posts and comments that Hydra
displays, the user authors, or the user interacts with.

```text
library/
  index.yaml
  posts/
    <sha256-of-stable-anchor>/
      post.md
      revisions/
        <timestamp>-<content-hash>.md
      comments/
        <sha256-of-stable-comment-anchor>/
          comment.md
          revisions/
  comments/                 # comments whose root is not locally known
  norms/
  media/                    # searchable YAML manifests for preserved media
  tombstones/               # deletion requests observed before their content
  history/
    <UTC-date>.yaml
  backups/
    <UTC-date>/             # automatic pre-overwrite text copies
```

Every canonical `.md` file begins with YAML frontmatter containing its stable
anchor, author, protocol identifier, source URL when available, communities,
timestamps, content hash, parent/root relationships, interaction history, and
tombstones. The Markdown body then contains the title and public content.

Writes use a temporary file, file synchronization, atomic replacement, and
directory synchronization. A changed body moves the prior canonical document
into `revisions/`; a deletion request adds a tombstone and never removes prior
content. New canonical text is copied into that day's automatic backup, and
before any later overwrite the backup preserves the prior bytes.

The signed encrypted event ledger remains an operational index for private
persona state, relay delivery, and protocol evidence. When Hydra opens, a newer
or missing public object can be reconstructed from the readable library. This
keeps private keys and private messages out of plaintext while making the
public content library independently searchable and resilient.

Reddit API-fetched post and comment bodies are deliberately excluded. Reddit
content enters the library only when it is authored/imported as Hydra content
under an explicit user-controlled flow.
