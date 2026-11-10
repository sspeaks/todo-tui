# Jira-linked items

Once Jira sync is set up (todo-tui P2, design.md §5), Seth's assigned Jira issues are imported as ordinary items. Each linked item carries a `jira` field: `key` (e.g. `PLAT-530`, display only, changes if the issue moves), `url`, `instance` (e.g. `work-jira`), `tracker`, and `remote_status` (Jira's own status name, e.g. "In Review"). Before sync is set up no item has one, and nothing here applies.

## What reaches Jira

Sync pulls the issues in (every minute while the TUI is open, every ten otherwise) and **pushes some edits back** (ADR-0007). An edit that pushes changes Seth's real ticket, which his team sees, within seconds while Jira answers.

**Pushed to Jira**, through `todo_update` and `todo_set_status`:

- title, priority, labels, due date;
- the status: `done` closes the issue with resolution **Done**, `cancelled` with **Won't Do**, `in_progress` starts it, and `inbox`/`next`/`someday` from in progress or done moves it back to To Do. Between inbox, next and someday the move is local, while Jira has the issue in To Do.

**Jira's alone**, refused by the tools ("that field is Jira's: change it in Jira"): the description, assignee, components, fix versions, parent, story points and estimates. Sprint, reporter and Jira's status name are read-only too. Don't retry; tell Seth it changes in Jira, with the key and link.

If Seth's config has `push = false` (the kill switch), nothing pushes and every Jira field is refused as above.

**Local only**, never pushed. Prefer these for Seth's personal organization of a ticket:

- `local_priority`: his own ordering, without touching the ticket's priority. "Push it down my list", "it matters less to me" or "bump it up for me" on a Jira item means this, not `priority`. Afterwards `priority` shows the override, and `own_priority` the ticket's.
- `defer_until`: hide it until a date.
- `waiting_on`, `waiting_reason`, `follow_up`: who he's blocked on.
- `private_notes`: his notes, never visible in Jira.
- `energy`, archive.

## Asking first

A pushed edit changes a shared ticket, so:

- **Do what Seth asked, and nothing more in Jira.** "I finished PLAT-530" is `todo_set_status` done: it closes the issue as Done. Say so in the reply ("closed in Jira as Done").
- **Ask before closing or renaming as a side effect**: tidying titles, closing what looks stale, cancelling during a triage. "Clean up my inbox" never closes a Jira item on its own.
- **"Drop it", "not doing it"** on a Jira item is ambiguous: cancel (closes it in Jira as Won't Do) or delete locally (Jira untouched). Ask which.
- **Someone else's issue**: an edit of an issue assigned to someone else fails ("assigned to someone else … confirm_not_mine"). Ask Seth whether to change their ticket; only with his yes, call again with `confirm_not_mine: true`.

Repeat rules don't fit linked items: a fixed rule (`mon`, `daily`) sets the due date, which pushes, and each completion would close the issue. Suggest a local todo that refers to the key instead.

## After a change: pending, conflicts, refusals

- `pending_fields` on an item lists the fields waiting to reach Jira (offline, or not sent yet). They go on their own once Jira answers; say "queued for Jira" rather than "done in Jira" while they wait.
- `has_conflict`: Jira changed a field while his edit of it waited. Jira's value stands, and his is kept as a Suggestion.
- `push_failed`: Jira refused his change (a workflow that cannot close from there, a field it will not take). Jira's value stands.
- `todo_suggestions` lists both, with Jira's error and the values. Re-applying his value or letting it go is Seth's, in the TUI's Suggestions pane (`S`); the tools never apply one. Tell him what is there and point him to `S`.
- `todo_status` lists each instance under `sync`: `ok` with `last_pull`, `syncing`, `offline` (it retries), `auth_error` (the token was refused: Seth stores a new one, then `todo sync now`), or `degraded` with an `error`; `to_push` counts what waits and `refused` what Jira refused.
- `jira.unavailable: true` means the issue was deleted in Jira or he lost access; the item is archived (`archived: "remote_unavailable"`) and nothing pushes to it.

## Doing things

- **Find a ticket**: `todo_list` with `text: "PLAT-530"` matches the key. Always show the key and link when listing Jira items.
- **Delete is local only**: it hides the item in todo-tui and stops it being imported again, and cancels its edits still waiting to push, but never deletes or closes the issue in Jira, which stays open and assigned. Use it when Seth wants the ticket out of his todo list without touching it.
- **Comments, other transitions, reassigning and creating issues** aren't covered by the todo tools. Use a Jira tool if one is available, and say that's what you're doing.
