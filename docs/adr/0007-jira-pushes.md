# ADR-0007: Pushes to Jira, narrowly

- Status: accepted
- Date: 2026-11-05
- Amends: design 3.2 (which Core fields a person may change on a linked Item),
  3.4 (transitions), 5.6 (the outbox), 5.8 (resolutions), 8.2 (`[[instances]]`
  keys); follows ADR-0006

## Context

todo-tui is a terminal todo manager whose Rust daemon owns the data and whose
clients (an Ink TUI, the `todo` CLI, an MCP server) are thin; its design is
[`docs/design.md`](../design.md). ADR-0006 made Jira **Data Center** the first
tracker and its first milestone (M9) **read-only**: issues are imported and kept
fresh, and a linked Item refuses every edit to a field Jira owns (`LINKED_READ_ONLY`),
because the next pull would silently overwrite it.

That leaves the most common act undone: finishing an issue here does not finish it
in Jira. The design's answer (5.6) is an **outbox**: a local edit to a Jira-owned
field commits locally at once and is queued, a worker pushes it, and a change made in
Jira meanwhile wins, the local value kept as a Suggestion to re-apply (3.11).

The author's instance shapes the details. Its project (BRME) has one workflow for its
issue types: Open, To Do and On Hold in the To Do category; In Progress (reached by a
transition named "In Progress (2)") and Blocked in In Progress; and **Closed**, the only
way into Done, which **requires a resolution** chosen from about 55. Over the last year
the project's 3,479 resolved issues were Done 2,292, Won't Do 308 (steady month to
month), Overcome by Events 287 (190 of them in one two-week clean-up), Incomplete 168
(mostly unfinished stories at sprint ends), Duplicate 148, Completed 88 and Fixed 48.

## Decision

**M10 pushes a few fields, through the outbox.**

- **Pushed**: the status (done, cancelled, in progress, back to To Do, as Jira
  transitions) and the title, priority, labels and due date.
- **Still read-only**: the description (it needs a wiki-markup converter that can tell
  when it would lose formatting, design 5.6), the assignee (it needs People, 3.5), and
  components, fix versions, story points, estimates and the parent. The guard keeps
  refusing these, and refuses everything on a link that is `broken`. (M11 pushes the
  description too, converted to and from wiki markup with nothing dropped: ADR-0009.)
- **A transition** is chosen per Item by the target status category: the first one
  Jira offers into it, in Jira's order, or the one config names by its target status
  (`transitions = { todo = "Open" }`). Closing sets the resolution the transition
  screen requires: `done_resolution` (default **Done**) for done, `cancel_resolution`
  (default **Won't Do**) for cancelled, sent as the value Jira lists (its id), whatever
  the config's case. These defaults are the project's own precedent above. A close must
  read back as itself, so a `cancel_resolution` missing from `cancelled_resolutions`
  joins it, and a `done_resolution` in it gives way to Done, each with a warning. A
  transition to To Do or In Progress never sets a resolution: an optional one on its
  screen (a reopen) is left empty, and a required one fails the push, since it would
  leave the issue resolved.
- **Priorities** map both ways from one table: by rank, then `priority_map`, so a
  level pushed reads back as itself; a level with no Jira priority of its own pushes
  as the nearest one, and takes the level that one reads back as (High on a Jira of
  High, Medium and Low becomes Highest), so its own push is no conflict.
- **Jira wins, yours is kept.** Each queued patch keeps the value it was made against
  (its base). When Jira's value has moved away from the base and is not the local one,
  Jira's value is applied and the local one becomes a `sync_conflict` Suggestion, one
  key to re-apply. A push Jira refuses (400, 403, no transition) restores Jira's value
  and becomes a `push_failed` Suggestion with the reason. The fields (one `PUT`) and
  the status (one transition) are two units, each settled or refused on its own: a
  refused close does not undo a title Jira took. A reopen goes before the fields, as a
  closed issue may take no edits.
- **In flight.** The worker marks what it sends (each row's `sent`) before the call.
  An edit made meanwhile changes only what waits: once Jira reads back at what was
  sent, it is pushed from there, so a quick second edit is neither lost nor a conflict
  with the first.
- **Backoff.** A transient failure (offline, 5xx, 429) ends the pass, as a failed pull
  does, rather than trying the rest of the batch; the rows retry with backoff from
  15 s, and the loop wakes when the first is due. A 429's `Retry-After` holds the
  whole instance, pulls included.
- **Jira has no conditional update** (no `If-Match`), so the worker reads the issue
  again just before writing and runs the rule on that. The window left between the
  read and the write is accepted for a personal tool. A transition is not idempotent:
  a retry after a lost answer reads first, and finds it done.
- **`push = false`** on an instance keeps M9's read-only behaviour, a switch for a
  Jira one should not write to. What waits stays queued, and pulls show Jira's values
  meanwhile; it goes once pushes are on again (or an undo or delete cancels it). An
  undo or redo made meanwhile queues its value too, rather than drift from Jira
  silently. A reload applies the switch at once, mid-pass.
- **`local_fields = ["due_date"]`** on an instance makes its due date the person's:
  an edit is taken with pushes on or off, queues nothing and asks nothing for someone
  else's issue, and pulls neither import nor overwrite it. It is for a Jira whose
  screens refuse the due date (BRME tracks a custom "Need Date" instead). Only the
  due date can be local for now.

## Alternatives considered

- **Write-through** (call Jira inside the RPC). Every edit would wait on the network
  and fail offline, against design 5.1's "local writes never block".
- **Local wins.** A teammate's change in Jira would be overwritten by an edit made
  offline, often without either noticing.
- **Ask for a resolution at each close.** A picker of 55 on every `x`, for a choice
  the project makes the same way two times in three.
- **Push everything editable now**, the description included. Wiki markup round
  trips are where data gets lost; they deserve their own milestone and tests.

## Consequences

- A linked Item's pushed fields are editable everywhere (TUI, CLI, MCP); the rest
  still answer `LINKED_READ_ONLY`, now naming what is Jira's only.
- Undo of a pushed edit is a new edit, pushed in turn.
- The Suggestions pane arrives, holding only sync kinds for now (Claude's come in P3).
- A transition that needs more than a resolution on its screen fails as `push_failed`
  naming the field, until such screens are handled.
