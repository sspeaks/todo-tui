---
name: todo
description: Manage Seth's personal todo list in todo-tui — capture tasks, list what's due or on deck, mark things done, triage the inbox, set due dates, labels, priorities, waiting-on and repeat rules, and review Jira-linked items. Use this whenever the user wants to add, find, change, complete, defer, schedule or review a todo, task, reminder, follow-up or chore ("remind me to…", "add a todo", "what's on my plate", "what's due today", "mark X done", "I'm waiting on Dana for…", "triage my inbox", "every Monday…", "what Jira tickets do I have"), even when they don't say "todo". Not for editing the todo-tui source code itself.
---

# Managing todos with todo-tui

Seth's todos live in todo-tui: a local daemon owns them, and the TUI, the `todo` CLI and an MCP server are all clients of it. Every change any client makes shows up live in the others and lands on one shared undo stack.

## Pick the interface

1. **The `todo_*` MCP tools** (from `todo mcp`), when they're available. They can do everything: capture, list, get, update, set status, set a repeat rule, archive, delete, undo.
2. **The `todo` CLI** as a fallback. It can only capture, undo/redo and count:
   - `todo add '<capture text>'` — always single-quote it, since the shell eats `#`, `>`, `~` and `^`. Several items: one per line on stdin (`printf '%s\n' 'a +2' 'b #home' | todo add`).
   - `todo undo`, `todo redo`, `todo status`.
   If the user asks for something the CLI can't do (list, complete, edit), do what you can and tell them once that `claude mcp add --scope user todo -- todo mcp` enables the rest.

Either way the daemon starts itself if it isn't running.

## Capturing

Capture text carries fields as inline tokens, so one call does the whole job. Prefer `todo_capture` with tokens over a capture followed by `todo_update`:

| Token | Meaning |
|---|---|
| `+1` … `+5` | Priority, 1 = highest. Map "urgent/high" → `+1`/`+2`, "low" → `+4` |
| `^<date>` | Due: `today` `tom` `fri` `+3d` `+2w` `eow` `eom` `nov2` `2026-11-02` |
| `#label` | Label, repeatable |
| `~person` / `~"two words"` | Waiting on someone: links the one known person the name means, else free text |
| `>next` / `>someday` | Status instead of the default inbox |
| `@rule` | Repeat: `@daily` `@weekdays` `@mon,thu` `@weekly` `@monthly` `@15th` `@3d` `@2w` |
| `\` | Escapes a sigil that belongs in the title (`fix \#1 fan`) |

Write a short, verb-first title and keep the user's wording. Put dates and people in tokens, not in the title ("Send Priya the Q4 deck +2 ^fri #work", not "Send Priya the Q4 deck by Friday"). Weekday names mean the next one strictly after today, so `^fri` on a Friday is a week out; use `^today` for today. For a phrase the grammar has no word for ("end of next week", "the Tuesday after next", "mid-December"), work out the date from today's (`todo_status` gives it) and pass it as `YYYY-MM-DD`. "End of next week" is next week's Friday, not `eow`. A token that doesn't parse stays in the title and comes back as a warning — when that happens, fix it with `todo_update` and mention it.

For the full grammar (date forms, how `@weekly` takes its day from `^`), read `references/capture-syntax.md`.

## Views

Items have a status: `inbox` (just captured, untriaged), `next`, `in_progress`, `someday`, `done`, `cancelled`. The saved views, in tab order:

- **Inbox** — status inbox, oldest first. Things to triage.
- **Today** — due today or earlier, in progress, a waiting item whose follow-up is due, or focused this week. Overdue first, then by priority.
- **Next** — next and in progress, by priority then due date.
- **Waiting** — anything waiting on someone.
- **Someday** — parked ideas.
- **Archive** — done, cancelled and archived.

Deferred items (`defer_until` in the future) are hidden from every view until that day. So are done, cancelled and archived ones, unless `include_done` is set. `todo_list` with `text` searches titles, labels, descriptions and Jira keys. Start "what's on my plate" with `todo_status` (counts plus today's date), then list Today, then Next.

## Ids

Every tool returns `short_id` (8 characters, the same id `todo add` prints) and the full `id`, and every tool accepts either. Show short ids when listing so the user can refer to items. When the user names an item by its title, find it with `todo_list` `text` and act on the single match. If several open items match, change nothing and ask which one. List each match as `short_id title`, so the user can answer with an id.

## Common jobs

- **Complete**: `todo_set_status` `done` (or `cancelled` for "never mind"). For a repeating item the result's `next` is the next instance — tell the user its due date.
- **Triage the inbox**: list Inbox and decide each item's status (`next`, `someday`, `done`, `cancelled`), plus labels, priority or due date where they're obvious. Show the plan as a short table and apply it after the user agrees, unless they already told you the rules ("cluster stuff to next with #brgs, everything else someday" is enough to just do it).
- **Waiting on someone**: `todo_update` with `waiting_on` (and `follow_up` for when to chase), or capture with `~person`. Stop waiting with `waiting_on: null`. A coworker is a Person (from Jira), so their name and address reach a meeting link: find them with `todo_people` and pass `waiting_on: "person:<person_id>"`. `~dana` links on its own when one person answers to it; when two do, `todo_capture` says `waiting_matches` and keeps the text, so ask Seth which one he meant. `todo_people` with `jira: true` searches Jira's directory for someone not known yet; it does not add them (picking them in the TUI's `w` does). Free text (`~"the vendor"`) is fine for anyone who is not a person in Jira.
- **Not now**: `defer_until` hides it until a date; status `someday` parks it indefinitely.
- **Repeat**: `todo_set_repeat`, or `@rule` at capture. For what completing, skipping and deleting do to a series, read `references/recurrence.md`.
- **Review**: list Today and Next, call out overdue items and stale waiting items, and suggest — don't apply — changes.

## Being careful

- Every change goes on one shared undo stack. If you get something wrong, `todo_undo` reverts the most recent change, whether it came from you, the TUI or the CLI. Say so after a bulk change.
- Prefer archive or `done` over delete. Delete is soft (`todo_undelete` brings it back), but deleting the open instance of a repeating item ends the series.
- Confirm before changing more than a handful of items, unless the user gave explicit rules.
- `todo_update` `labels` replaces the whole set. To add one, read the item's labels first and send them all.
- If a tool says the item "changed elsewhere", Seth edited it in the TUI meanwhile. Get it again and retry.

## Claude's refines

When the assistant is on, Seth can ask Claude to refine an item (`space r` in the TUI, `todo refine <id>`): a clearer title, due date, priority, labels, waiting-on. The proposal is a `refine` Suggestion in `todo_suggestions`; applying it is his, in the TUI (S). You are Claude too: if he asks you to clean up an item, just edit it with `todo_update`; don't route it through refine.

## Jira-linked items

An item with a `jira` field (key, url, remote_status) mirrors a Jira issue. Some edits of it are **pushed to Jira**: they change the real ticket that Seth's team sees. Read `references/jira.md` before changing one. In short:

- The title, description, priority, labels, due date and status push. `todo_set_status` done closes the issue with resolution Done, cancelled with Won't Do. Do these only when Seth asks for that change, and ask before renaming or closing a ticket as a side effect of something else.
- A description is Markdown; keep its `wiki:` code spans and ```` ```wiki ```` blocks as they are (Jira formatting with no Markdown form).
- The assignee and the other Jira fields never push: the tools refuse them. Say it changes in Jira, and give the link.
- For Seth's own ordering and scheduling, prefer the local fields: `local_priority`, `defer_until`, waiting, energy and `private_notes`.
- An issue assigned to someone else is refused unless Seth agrees (`confirm_not_mine`). Ask him first.
- To take a ticket off his list without touching it in Jira (reassigned, not his any more), delete it locally.

## Replying

Keep replies short and scannable. After a change, confirm in a line with the short id and the fields that matter (`MTTVN5Y1 Send Priya the Q4 deck · due Fri Nov 6 · +2 #work`). For lists, group by view or due date and show short id, title, due, and labels or Jira key. Don't dump raw JSON.
