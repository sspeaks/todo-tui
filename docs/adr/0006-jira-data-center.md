# ADR-0006: Jira Data Center first

- Status: accepted
- Date: 2026-11-03
- Amends: design 5 (the sync engine: 5.2, 5.3, 5.4, 5.8, 5.10, 5.13), 8.2
  (`[[instances]]`), 9.2 (open question 11)

## Context

todo-tui is a terminal todo manager. A Rust **daemon** owns the SQLite database and
serves a gRPC API; the clients (an Ink **TUI**, the `todo` CLI, an MCP server for
Claude Code) are thin. Its design is [`docs/design.md`](../design.md); section numbers
below are that document's.

P1 built everything local. **P2** is **Jira sync**: issues from a Jira instance become
Items and stay in step with Jira both ways (design 5). The design keeps every
tracker-specific detail in an **adapter** behind a Rust trait (5.2), so the engine
(cursors, the outbox, conflicts, scopes) never knows which tracker it talks to.

The design was written for **Jira Cloud**: email and API token auth, REST API v3 with
page-token search, descriptions as ADF (a JSON document format), users named by
`accountId`. Open question 11 left self-hosted **Jira Data Center** (formerly Server)
"unsupported, on demand". The author's Jira, the one P2 is built to use, is Data
Center.

The two products serve the same issues, but differ in the parts an adapter touches:

| | Cloud | Data Center |
|---|---|---|
| Auth | email + API token (basic) | Personal Access Token, `Authorization: Bearer <PAT>` |
| Search | `/rest/api/3/search/jql`, a page token | `/rest/api/2/search`, `startAt` / `maxResults` (at most 1000 a page) |
| Descriptions | ADF | wiki markup (`*bold*`, `h2.`, `{code}`) |
| A user | `accountId` | `key` (stable; `name` can be renamed) |
| Timestamps | ISO 8601 | ISO 8601 with a colon-less offset (`+0000`) |

## Decision

**The first adapter is Data Center** (`kind = "jira-dc"` in `[[instances]]`), in
`crates/todo-jira`. It authenticates with a Personal Access Token from the credential
store (5.10), searches with REST v2 and `startAt` paging, and names users by `key`. JQL
times are minute-precise and in the Jira user's own time zone, which `/myself` reports,
so the cursor is kept as a UTC instant and converted for each query (as 5.3 says for
Cloud).

**The first P2 milestone (M9) is read-only.** Issues are pulled, imported, kept fresh
and reconciled; nothing is written to Jira. A linked Item's Core fields cannot be edited
locally until pushes exist, since the next pull would overwrite the edit silently
(5.1): its Overlay fields (waiting, defer, notes, energy, the local priority override)
stay editable. Descriptions are stored as the raw wiki text until pushes need a
converter.

Everything else in design 5 stands: the engine and the trait are tracker-agnostic, so
a Cloud adapter is a second implementation of the same trait, chosen by `kind`.

## Alternatives considered

- **Cloud first, as designed.** It could not be used or tested against the Jira P2 is
  for, so its first real run would come long after it was written.
- **One adapter for both, switching on the server's version.** The auth, paging and
  rich-text code would all branch, and neither half could be tested alone. Two
  adapters behind the trait keep each one plain.
- **Two-way sync in the first milestone.** Twice the size, and its first live test
  would write to a work Jira. Read-only first proves the pull side on real data
  before anything is pushed.

## Consequences

- Open question 11 is answered for Data Center; Cloud waits for a need.
- ADF conversion (open question 3) is not needed yet; wiki markup conversion is, once
  pushes arrive.
- While M9 is the latest milestone, a linked Item refuses Core edits with
  `LINKED_READ_ONLY`, and the clients say why.
