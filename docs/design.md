# todo-tui — Design Document

> Status: Draft v1 (reader-tested, 2 rounds) · Owner: sspeaks · Last updated: 2026-10-25
>
> Audience: an implementing agent who will design the gRPC APIs and then build the system. This doc defines *what* and *why*; API and schema details are deliberately left to the next phase except where they constrain the design.

## 1. Overview, Goals & Non-Goals

### 1.1 What this is

`todo-tui` is a personal, keyboard-driven todo system for the terminal. It has three parts:

- A **Rust daemon** that owns all data and logic, stores it locally in SQLite, and keeps linked items synced with an issue tracker (**Jira Cloud first**; GitHub, Azure DevOps and others later).
- An **Ink (React) TUI in TypeScript** that talks to the daemon over **gRPC on a Unix socket**, plus a CLI one-liner and an MCP server for Claude Code.
- Optional **Claude** integration that *proposes* refinements, merges, priorities, splits, reviews and drafts. You accept proposals one checkbox at a time.

The core idea is GTD-style **capture without friction**: dump a thought in seconds and trust the system to hold it. Then **clarify it later**, with help, into a well-formed item that can live in Jira when it belongs there.

### 1.2 Goals

1. **Frictionless capture**: under 3 seconds from any terminal or from Claude Code. Works offline. Never fails (2.1).
2. **Keyboard-speed triage and editing**: every common field change is one or two keys from the list (2.5).
3. **Jira is the source of truth for linked items**: sync is automatic and best-effort, conflicts resolve in Jira's favor, and **nothing you typed is ever silently lost** (5.1).
4. **Personal workflow on top of Jira**: waiting-on, deferral, someday, focus and private notes live locally, even on Jira items (3.2).
5. **Claude helps but never acts alone**: every AI output is a Suggestion reviewed in one pane (3.11, 2.6).
6. **Nothing stagnates**: stale detection and a weekly review keep the backlog honest (6.5).
7. **Future-proof**: the daemon owns all logic behind a versioned, additive gRPC contract, and trackers, Claude providers, clients and views are all pluggable (4.3, 8.1).
8. **Offline-first, local, exportable**: full functionality offline except network features, plus lossless machine-readable export (7.3, 7.4).
9. **Open-source ready**: no work data in the repo, permissive license, conformance-tested adapters (8.3).

### 1.3 Non-goals (v1)

Multi-device sync (other than through Jira) · team or multi-user features · web, mobile or desktop GUI clients · Jira Server/Data Center · OAuth · editing sprints or unmodelled custom fields · attachment contents · OS notifications and reminders · Windows · out-of-process adapter plugins · human-readable export · trackers other than Jira (until P5).

### 1.4 Glossary

| Term | Meaning |
|---|---|
| **Item** | The single todo entity, from a raw capture to a full Jira issue (3.1) |
| **Core / Overlay / Remote extras** | The three field layers; they determine who owns a field (3.2) |
| **Linked** | An Item with a `RemoteLink` to a tracker issue (3.3) |
| **Instance** | One configured tracker connection, e.g. `work-jira` (5.3) |
| **Scope** | A named tracker query that decides which issues are imported automatically (5.3) |
| **Snapshot** | The last pulled remote state of an Item; the base for conflict detection (5.4) |
| **Outbox** | Durable queue of pending pushes to a tracker (5.6) |
| **Suggestion / Patch** | A proposed change set / one checkbox-able field change within it (3.11) |
| **Tombstone** | A soft-deleted Item; blocks re-import and resolves merge aliases (3.7) |
| **Tracked set / pinned** | All linked Items, kept fresh by id regardless of scope / linked Items you promoted or attached, never archived for leaving scope (5.3) |
| **Event / `op_id`** | Recorded field change / the group of Events from one user action, the unit of undo (3.10) |
| **Revision** | Global counter bumped on every write; drives the change stream (4.4) |
| **Task kind** | One Claude feature: prompt, schema, trigger, model (6.2) |

### 1.5 How to read this document

Section 3 (the domain model) is the foundation, and everything else references it. The API design phase should start from 4.3 (the contract rules) and 4.4 (the change stream), deriving messages from Section 3 and RPCs from Sections 2, 5 and 6. Section 9 lists what is deliberately undecided.

## 2. User Experience

The TUI is designed for two speeds: **capture in under three seconds**, and **triage at the speed of keystrokes**. Every common action is one or two keys from the list, with no forms. Vim conventions throughout.

### 2.1 Capture paths

| Path | How | Notes |
|---|---|---|
| **CLI one-liner** | `todo add "call dentist ^fri +2"` (alias `todo a`) | Prints the short id and parsed fields, then exits. `todo add` with no argument reads stdin, **one capture per line**, for pasting a list or meeting notes. At a terminal each line is captured as it is entered, so Ctrl-C loses only lines not yet captured (normally just the one being typed); piped input is read whole first. |
| **TUI quick-add** | `a` from any view | Inline input at the top of the list, with a **live preview** of parsed tokens as you type. `enter` saves and stays open for the next capture; `esc` closes. |
| **MCP** | From Claude Code: "add a todo to fix the flaky test" | Uses the same capture call; `created_via = mcp`. |

**Capture never fails for want of a daemon.** If the daemon cannot be reached or started within the auto-spawn budget, the CLI appends a line `{ "spool_id": <ULID>, "captured_at": <local timestamp with offset>, "text": "…" }` to `spool.jsonl` in the data directory (7.1), using an exclusive append lock, and exits successfully. The daemon imports the spool when it starts and every 30 s while it runs: each line with `ParseCapture(text, reference_time = captured_at)` so relative dates resolve against the capture time, `created_via = cli`, skipping any `spool_id` it has already imported (crash-safe). The file is truncated after a successful import; lines it cannot import (malformed, blank, or over the capture limit, 2.3) move to `spool.rejected.jsonl`. Text over the limit is neither sent nor spooled: the CLI reports it `NOT CAPTURED` and exits 1. Captures are always local first (`inbox`); nothing touches Jira until you promote.

**Shell prompt integration**: `todo status --short` prints compact counts (e.g. `📥3 ⏰1 ✦2` = inbox items, overdue-or-due-today items, pending suggestions; zero counts omitted) for prompt or tmux status lines. It must answer from the daemon in well under 50ms, or print nothing.

Future capture surfaces (Raycast, macOS Shortcuts, mobile) are just more gRPC clients. No design change is needed.

### 2.2 Layout

```
┌ 1 Inbox 3 │ 2 Today 5 │ 3 Next 12 │ 4 Waiting 4 │ 5 Someday │ 6 work-jira 18 │ 7 Archive ────┐
│ ▸ ○ +2 Fix flaky login integration test     PLAT-530  ^Fri  ⧗dana 3d  ✦  │ PLAT-530 · Bug        │
│   ○ +3 Renew passport                                 ^Nov 2            │ Fix flaky login …     │
│   ◐ +1*Ship auth migration                  PLAT-412  ^Today       ↑    │ Status   In Review ◐  │
│   ○    Call dentist                                                     │ Priority Medium (+3)  │
│                                                                          │ Waiting  Dana · 3d    │
│                                                                          │ Due      Fri Oct 24   │
│                                                                          │ ── Description ──     │
│                                                                          │ …                     │
├──────────────────────────────────────────────────────────────────────────┴───────────────────────┤
│ NORMAL │ work-jira ● synced 40s ago │ ↑1 pending │ ✦ 2 suggestions │ ? ask  : command  F1 help  │
└────────────────────────────────────────────────────────────────────────────────────────────────┘
```

- **Tab bar**: the views (2.4) with counts. `1`–`9` jump to a view; `[` / `]` go to the previous / next view.
- **List**: one dense row per Item: status glyph (`○` todo, `◐` in progress, `●` done, `⊘` cancelled), *effective* priority (`*` suffix = local override, 3.2), title, Jira key, due, waiting (person + age), and markers: `✦` pending suggestions, `↑` pending push, `!` sync conflict / push failure, `🔁` recurring.
- **Detail pane** shows the Jira status *name* when linked (e.g. "In Review") with the category glyph; the local status otherwise.
- **Right pane**: shows either the **detail** of the selected Item, or the **Suggestions** drawer (`S` toggles). It follows the list selection.
- **Status bar**: mode, per-instance sync state (5.11), pending outbox count, global suggestion count, key hints.
- **Responsive**: at ≥ 110 columns, list and right pane sit side by side (~60/40). Below that, the list is full width and detail/suggestions open as full-screen overlays (`enter` / `S`, `esc` to close). Long titles truncate, never wrap, in the list.

### 2.3 Inline capture syntax

Parsed by the core (Rust) and exposed as a `ParseCapture(text, reference_time?)` RPC (reference time defaults to now), so CLI, TUI preview, MCP and spool import behave identically. A token is a whitespace-delimited word starting with a sigil; tokens never contain spaces except the quoted `~"…"` form. A token that fails to parse **stays in the title** and the parse result carries a warning (shown in the TUI preview, printed by the CLI). Everything else is the title. `\` escapes a sigil (`\#1 fan`). Capture text is at most **64 KiB** (65536 bytes of UTF-8): `CaptureItem`, `ParseCapture` and the spool import refuse longer text (`INVALID_ARGUMENT` on `text`), and an update refuses a longer title (`item.title`); description has no limit of its own.

On a shell command line, single quotes are the safe form. Unquoted, bash treats `#label` as a comment and `>next` as a redirect, zsh fails on `~name` ("no such user or named directory"), and zsh with `extendedglob` expands `^fri` as a pattern. Double quotes protect the sigils but still expand `$`.

| Token | Meaning | Examples |
|---|---|---|
| `+1` … `+5` | Priority: 1 = highest … 5 = lowest | `+2` |
| `^<date>` | Due date (grammar below) | `^fri`, `^tomorrow`, `^+3d`, `^2026-10-24`, `^nov2`, `^eow` |
| `#<label>` | Label (tag) | `#auth #oncall` |
| `~<person>` | Waiting on. Fuzzy-matched against known People; unknown names are free text | `~dana`, `~"the vendor"` |
| `>someday` / `>next` | Initial status refinement (default `inbox`) | `>someday` |

**Date grammar** (shared by `^`, the `^` / `D` prompts, and follow-up dates), case-insensitive, resolved in local time against the reference time:

| Form | Meaning |
|---|---|
| `today`, `tod`, `tomorrow`, `tom` | As named |
| `mon` … `sun` (3+ letters) | Next occurrence **strictly after** today (`^fri` on a Friday = next week's Friday) |
| `+Nd`, `+Nw`, `+Nm` | N days / weeks / calendar months from today |
| `eow`, `eom` | This week's Friday (next Friday if today is Sat/Sun) / last day of this month |
| `YYYY-MM-DD` | Absolute |
| `<mon><d>` e.g. `nov2`, `nov02` | Next occurrence of that month/day (this year if not passed, else next year) |

### 2.4 Views (tabs)

Views are **saved queries**: a server-side filter + sort (4.3 rule 6). The defaults below are just preconfigured saved views, and you can add, remove and reorder views in config.

| # | View | Shows | Default sort |
|---|---|---|---|
| 1 | **Inbox** | `inbox` | Oldest first (process in order) |
| 2 | **Today** | Overdue or due today, `in_progress`, waiting follow-ups due, `focus_week` | Overdue → priority → due |
| 3 | **Next** | `next` + `in_progress` | Priority → due |
| 4 | **Waiting** | `waiting` flag set | Longest waiting first |
| 5 | **Someday** | `someday` | Last touched |
| 6 | **<instance id>** (e.g. `work-jira`), one tab per instance | Open (not done/cancelled) Items linked to that instance | Remote key |
| 7 | **Archive** | Archived (any reason), dimmed | Archived date desc |

**Implicit rules** applied to every view unless its filter explicitly opts in: exclude deleted; exclude archived; exclude `done`/`cancelled`; exclude `defer_until` in the future. Archive opts in to archived (and done/cancelled). `/` search filters within the current view; `:search …` searches everything including archived.

Views are stored **in the database** (not the TOML), created and edited with `:view new|edit|delete`, ordered by `:view move`; the defaults above are seeded on first run, and when an instance first appears in config its tab is appended once (a deleted instance tab is not re-created). The filter language is defined in 4.8.

### 2.5 Keymap (normal mode)

All bindings live in one declarative keymap table (4.6), are listed in the `F1` help overlay, and are user-overridable.

**Navigation**: `j`/`k` move · `gg`/`G` top/bottom · `ctrl-d`/`ctrl-u` half-page · `tab` cycle focus list ↔ right pane · `1`–`9`, `[`, `]` views · `/` incremental filter (`n`/`N` next/prev match) · `esc` clear / close.

**Item actions** (on the selected Item, or on all marked Items):

| Key | Action |
|---|---|
| `a` / `A` | Quick-add / quick-add as child of selected |
| `enter` | Open detail (field-by-field editing: `j`/`k` over fields, `enter` edits one) |
| `i` | Edit title inline |
| `E` | Edit description in `$EDITOR` |
| `p` then `1`–`5` / `0` | Set priority (the Core field: pushed to Jira when linked) / clear (`0` is rejected with a message if the tracker requires a priority). The local override is set from the detail pane |
| `^` | Due date prompt (same date grammar as the `^` capture token) |
| `D` | Defer until… |
| `w` / `W` | Set waiting-on (person prompt + optional follow-up date) / clear waiting |
| `s` | Status picker (`inbox`, `next`, `in_progress`, `someday`, `done`, `cancelled`) |
| `x` / `X` | Mark done / cancelled |
| `t` | Edit labels |
| `P` | Promote to Jira (target from config; shows missing required fields on failure, 5.7) |
| `L` | Link (attach) to an existing Jira issue by key |
| `o` | Open in browser (Jira URL) |
| `z` | Archive / unarchive |
| `dd` | Delete (local only; confirm). `d` is reserved as a prefix, so it is not bound alone |
| `m` / `V` | Mark / visual range-mark for bulk actions |
| `u` / `ctrl-r` | Undo / redo (per `op_id`) |
| `gs` | Sync now |

**Claude actions** (`space` leader; hidden if no provider configured): `space r` refine · `space p` prioritize · `space s` split · `space c` draft comment · `space l` find related. All produce Suggestions. `?` opens the **Ask bar** (6.7).

**Command line** (`:`) with fuzzy completion, which doubles as the command palette: `:review`, `:standup`, `:sync`, `:promote`, `:link PLAT-123`, `:export`, `:view new …`, `:help`.

**Editing safeguards**: Core edits on a linked Item assigned to someone else ask for confirmation first (5.9). Editing a lossy description warns first (5.6).

### 2.6 Suggestions pane

The single place where you review AI proposals **and** sync fallout:

```
✦ Suggestions (2 items, 7 changes)                         enter toggle · ctrl-s apply · r reject
▾ Fix flaky login integration test · refine · "Inferred from your note"
   [x] title       follow up w/ Dana re… → Fix flaky login integration test
   [x] type        Task → Bug
   [ ] priority    — → +3
   [x] waiting     — → Dana (since today)
   [x] relate      ↔ PLAT-412 Auth test suite timeouts
▾ PLAT-530 · sync_conflict · "Jira changed priority while you were offline"
   [ ] priority    +1 (Jira) → +2 (yours)
```

- `j`/`k` move between suggestion headers and patch rows. **`enter` toggles the checkbox** under the cursor (on a header: toggles all its *ungrouped* patches). Ungrouped patches start **checked** for `source: claude` and **unchecked** for `source: sync` and `source: rules` (re-applying a conflict is opt-in). Grouped patches never start selected.
- Patches in an **alternative group** (3.11) render as radio buttons `( )` / `(•)`: checking one unchecks the others; none is pre-selected.
- `ctrl-s` (or `:w`) **applies all checked patches** across all suggestions in one undoable operation. Patches that became stale since rendering are skipped, not applied, and the toast says how many. Unchecked patches stay pending.
- `r` rejects the suggestion under the cursor; `R` regenerates it (Claude kinds only). `e` edits a proposed value before applying (recorded for learning, 6.8).
- Stale patches (3.11) are shown struck through and cannot be checked.
- Scope: `S` on an Item filters the pane to that Item; `:suggestions` shows everything.

### 2.7 Weekly review mode

`:review` (or the prompt shown at first launch on the review day) enters a full-screen guided flow through the five steps in 6.5. Each step is a list + Suggestions pane pair using the same keys as above. `]` / `[` go to the next / previous step, and the progress indicator shows the step. Exiting early saves progress (a review session record: week, current step, Items handled), and `:review` resumes it.

### 2.8 Feedback and errors

- Every mutation gives immediate visual feedback (optimistic, from the local daemon) with a one-line toast for anything non-obvious ("Promoted → PLAT-531", "Undid: mark done (3 items)").
- Errors never block: failures become `!`-marked Items plus Suggestions (`push_failed`), and auth problems show in the status bar.
- Background events don't move the selection or scroll position. New rows appear in place, briefly highlighted.

## 3. Domain Model

### 3.1 One entity: the Item

Everything is an **Item**. A two-second capture ("call dentist") and a fully specified Jira bug are the same type at different levels of completeness. A capture is just an Item with a `title` and `status = inbox`; every other field is optional. There is no separate "quick note" type to migrate later.

Every Item has:

- **`id`** — a local ULID, assigned at creation, never changes, never reused. All local references (relations, events, suggestions) use it.
- **`remote_link`** (optional) — present once the Item is linked to a tracker issue. See 3.3.
- **`created_via`** — `tui | cli | mcp | remote_import | restore | split | recurrence`, for filtering and for the standup.
- **`created_at`, `updated_at`** — local timestamps (distinct from `remote_*_at`).
- **`revision`** — the global revision (4.4) of the Item's last change; used as an etag by clients.

There is deliberately no separate "context" field (GTD `@phone` / `@errands`). Labels cover it (`#phone`).

### 3.2 Three field layers and ownership

Item fields fall into three layers. The layer determines **who owns the field**, and ownership is the single rule the sync engine (Section 5) follows.

| Layer | What it is | Owner when unlinked | Owner when linked |
|---|---|---|---|
| **Core** | Fields that map to standard tracker fields | Local | **Remote (Jira)** |
| **Overlay** | Fields with no tracker equivalent; personal workflow metadata | Local | **Local** (never pushed) |
| **Remote extras** | Tracker fields we don't model (custom fields) | n/a | Remote, read-only in v1 |

"Owner" means: on disagreement, the owner's value wins. For a linked Item, a local edit to a Core field is a *request* to change Jira; it is pushed via the outbox and only becomes authoritative once Jira accepts it (Section 5).

#### Core fields

| Field | Type | Notes |
|---|---|---|
| `title` | string | Jira `summary` |
| `description` | Markdown | Converted to/from Jira ADF; lossy for exotic ADF nodes (panels, macros). Lossiness must be detected so an unedited description is never re-pushed and corrupted. |
| `item_type` | enum-ish string | `task` by default; Jira issue type name when linked (`Bug`, `Story`, …) |
| `status_category` | `todo \| in_progress \| done` | See 3.4 |
| `resolution_kind` | `done \| cancelled`, only when category is `done` | Derived from the remote resolution when linked (5.8) |
| `remote_status_name` | string | Read-only, e.g. "In Review" (3.4) |
| `priority` | `none \| lowest \| low \| medium \| high \| highest` | Mapped to Jira priority names per instance (5.8). UI shorthand: `+1` highest, `+2` high, `+3` medium, `+4` low, `+5` lowest |
| `assignee`, `reporter` | Person ref | `reporter` read-only |
| `labels` | set\<string\> | Meaningful locally too |
| `components`, `fix_versions` | set\<string\> | Jira-only in practice; null when unlinked |
| `due_date` | date | |
| `parent` | Item ref | **The only representation of hierarchy** (epic / parent issue / sub-task / local parent); see 3.6 |
| `sprint` | string | Read-only in v1 |
| `story_points` | number | Mapped from an instance-configured custom field id |
| `original_estimate`, `remaining_estimate` | duration | |
| `resolution` | string | Read-only; informs `done` vs `cancelled` |
| `remote_created_at`, `remote_updated_at` | timestamp | Read-only |

#### Overlay fields (always local)

| Field | Type | Notes |
|---|---|---|
| `todo_refinement` | `inbox \| next \| someday` | Only meaningful while `status_category = todo`; retained (not cleared) otherwise, and reset per 3.4 when re-entering todo |
| `waiting` | `{ on: Person ref \| free text, reason, since: date, follow_up_on: date? }` or null | The "waiting on someone" flag. Orthogonal to status: an Item can be `in_progress` *and* waiting. |
| `defer_until` | date | Hidden from active views until this date |
| `private_notes` | Markdown | Never sent to Jira |
| `focus_week` | ISO week (e.g. `2026-W43`) or null | Set during the weekly review (6.5); the Item counts as "focus" while the week matches |
| `local_priority_override` | priority | Lets you rank an Item differently from its Jira priority without fighting your team. Views sort by *effective priority* = override if set, else `priority` |
| `archived` | `{ at, reason: user \| left_scope \| remote_unavailable \| auto_done }` or null | See 3.7 |
| `recurrence` | see 3.8 | |

#### Remote extras

A JSON map of remote field id → raw value, refreshed on every pull. Stored so that nothing is lost, they are visible in a detail view, and Claude can read them for context. Not editable in v1.

#### Derived and sync-state fields (read-only, computed by the daemon)

`status` (the 6-value convenience status, 3.4) · `effective_priority` (override if set, else `priority`) · `is_mine` (5.9) · `pending_fields` (Core fields with an un-pushed local value, 5.6) · `has_conflict` / `has_push_failure` · `description_lossy` and `description_dirty` (5.6) · staleness signals (3.10) · `scopes` (5.3) · `deleted_at` (3.7).

Read-only Core fields (`reporter`, `sprint`, `remote_status_name`, `remote_*_at`, `resolution_kind` when linked) and remote extras **cannot be targeted by any patch or update**; the daemon rejects such writes.

### 3.3 Remote link

```
RemoteLink {
  tracker_kind    // "jira" (others later)
  instance_id     // which configured site, e.g. "work-jira"
  remote_id       // stable numeric id — the join key
  key             // "PLAT-530" — display only; changes if the issue moves projects
  url
  last_pulled_version   // Jira `updated` timestamp at last successful pull
  link_state      // pending (promote queued) | ok | broken (remote deleted / no permission)
  pinned          // true if created by promote or attach: never archived for leaving scope (5.3)
}
```

`remote_id`, not `key`, is the identity, because keys change when issues are moved between projects.

An Item gains a link in one of three ways: **import** (an issue matched the configured auto-import query, Section 5), **promote** (you push a local Item to create a new Jira issue), or **attach** (you link an existing local Item to an existing Jira issue; Core fields are then overwritten by Jira's values, and any local Core values that differed are offered back to you as a Suggestion rather than silently discarded).

### 3.4 Status

Status is **stored as three fields** (3.2) and **exposed as one derived 6-value `status`**:

| Derived `status` | `status_category` (Core) | `resolution_kind` (Core) | `todo_refinement` (Overlay) | Jira status category |
|---|---|---|---|---|
| `inbox` | `todo` | — | `inbox` | To Do |
| `next` | `todo` | — | `next` | To Do |
| `someday` | `todo` | — | `someday` | To Do |
| `in_progress` | `in_progress` | — | (retained) | In Progress |
| `done` | `done` | `done` | (retained) | Done |
| `cancelled` | `done` | `cancelled` | (retained) | Done, with a resolution listed in `cancelled_resolutions` (5.8) |

Clients change status with one call, `SetStatus(item, status)`, which the daemon decomposes into the three fields. Consequences for a linked Item:

- **Jira owns the category and resolution.** If Jira moves the issue between categories, local status follows. Arriving into To Do from another category sets refinement to `next` — **unless** the move was our own transition: a transition op records its `target_refinement` (e.g. `SetStatus(someday)` on an `in_progress` Item), and when the pull observes that category change the recorded refinement is used instead.
- **Local owns the refinement.** Moving an Item from `next` to `someday` never touches Jira.
- **Changing category locally** (e.g. `next` → `in_progress`, or `x` / `X`) enqueues a Jira *transition*. Which transition to use per (project, target category) comes from instance config, with a default (first available transition into that category; for `cancelled`, one that can set a cancelled resolution). If no valid transition exists, or the transition screen requires fields, the op fails and a `push_failed` Suggestion offers a `choose_transition` alternative group (3.11) and/or the missing fields.
- The exact Jira status name (e.g. "In Review") is `remote_status_name`, read-only, shown in the detail pane.

`waiting` is a flag (3.2), not a status, because Jira will keep reporting "In Progress" while you wait on someone.

### 3.5 People

```
Person { id, display_name, emails[], remote_identities: [{ instance_id, account_id }] }
```

Assignee, reporter, and `waiting.on` reference People. People are created on demand from Jira data. `waiting.on` may instead be free text ("the vendor") so capture is never blocked on choosing a person.

### 3.6 Relations

```
Relation { from: Item id, to: Item id, kind: blocks | duplicates | relates_to }
```

- Hierarchy is **not** a relation: it is the Core `parent` field only (3.2). Local Items may have local parents (Claude's `split` creates local children); a linked Item's `parent` is remote-owned and maps to Jira's parent field (epics, parents and sub-tasks alike).
- If **both** ends are linked to the same Jira instance, the relation is remote-owned and maps to Jira issue links.
- If either end is local-only, the relation is local.
- Promoting a child whose parent is linked creates it as a Jira child/sub-task of that parent.
- **Identity** is the whole triple `(from, to, kind)`. `relates_to` is symmetric and stored canonically with `from < to`, so either spelling names the same relation.
- **Local relations (M2)** change only through `AddRelation` / `RemoveRelation`, never `UpdateItem`. Both ends must exist and differ; to *add*, both must be live. A relation is kept when an end is deleted, listed on both Items (`Item.relations`; a client sees the deleted end through `GetItem`), and can still be removed. Each change is one op that bumps both ends' revisions (the Item aggregate) and records one `relation_add` / `relation_remove` event on `from`; it is an undo entry like any edit (3.10). Adding an existing relation or removing a missing one changes nothing. No `expected_revision`: relations are a set. Not part of `BatchMutate` yet.

### 3.7 Archive, delete, merge

- **Archive** hides an Item from active views but keeps it fully synced. Reasons:
  - `user` — you archived it.
  - `left_scope` — an **unresolved**, unpinned issue no longer matches any scope (e.g. reassigned). If it later matches again, it is automatically unarchived.
  - `remote_unavailable` — the Jira issue was deleted or you lost permission. The link is marked `broken` and content is kept.
  - `auto_done` — `done` / `cancelled` Items are archived after `archive_done_after_days` (default 7; applies to local and linked Items alike — an issue resolved in Jira is *not* `left_scope`).
- **Delete** is local only and never deletes in Jira. It is a **soft delete**: the Item row keeps its content and gets `deleted_at` (+ `merged_into` for merges). A deleted Item *is* the tombstone: it is hidden everywhere, and its `(instance_id, remote_id)` blocks re-import. Deleting cancels the Item's pending outbox ops that are not in flight. **Undelete** (the same thing as "un-forget") clears `deleted_at` and restores the Item with its original id and overlay; it is also what `u` does after `dd`. Outbox ops cancelled by the delete are **not** restored (the toast says how many were dropped). A linked Item is immediately re-fetched by id (5.5 step 2 rules) so changes made in Jira while it was deleted are applied. Tombstones are exported.
- **Parents** (M2): **Delete** also soft-deletes every live descendant, in the same op and with the same `deleted_at`; **Undelete** restores the descendants deleted by that same op, not those deleted before it. Undeleting an Item whose parent is deleted fails with `FAILED_PRECONDITION` / `PARENT_DELETED` ("undelete the parent first"); a `BatchMutate` is checked as it leaves the Items when it ends, so the order of its undeletes does not matter. Invariant: **no live Item has a deleted parent**. No depth limit, no status roll-up; archive does not cascade. Live Items that an M1-era database left under a deleted parent (before deletes cascaded) are not repaired, and ops that touch them do not fail on it.
- **Merge** (from Claude's "this capture duplicates an existing item", Section 6): applied in this order within one op: (1) accepted `set_field` patches on the target; (2) any accepted Jira-side patches on the source (a "duplicates" link, resolving the source as cancelled), which are enqueued while the source still exists; (3) the source is soft-deleted with `merged_into = target`. `GetItem(source_id)` returns the target with `resolved_from = source_id`. Both histories are kept.

### 3.8 Recurrence

A recurrence rule (an RRULE subset: daily, weekly on given days, monthly by date, every N days after completion) lives on a local **series**. Completing an instance generates the next instance as a new Item. Recurrence is **local-only and never synced**. An individual instance can be promoted or attached to a Jira issue like any other Item; that does not link future instances.

### 3.9 Comments and attachments

- **Comments** `{ id, item_id, remote_id?, author, body (Markdown), created_at, state: synced | pending_push | draft }`. Synced comments are remote-owned and read-only. Drafts (including Claude-drafted status updates) stay local until you push them.
- **Attachments**: references only `{ name, url, size, mime }`. No local copies in v1.

### 3.10 Change history (events)

Every mutation appends an **Event**, alongside the current-state tables (the state tables are authoritative; events are not replayed to rebuild state):

```
Event { id (ULID), item_id, at, actor: user | sync | suggestion(<id>) | system, field, old, new, op_id }
```

`op_id` groups the events of one user action, which is the unit of **undo**. Events power undo, audit ("what did Claude change?"), the weekly review, and the staleness signals below.

**Undo semantics**:
- One **global** undo stack, shared by all clients (undo in the TUI can undo an MCP capture). An op is an **entry** when its actor is `user` or `suggestion(…)`, it is not itself an undo or redo, and it wrote at least one event: no-ops, replays and sync, system and scheduler ops never are. Spool-imported captures are user captures, so they are entries. A new entry clears the redo stack; nothing else does. Ops recorded before M2 are not entries. No depth or age limit.
- Undo reverts the **top** entry; there is no out-of-order undo. `Undo` / `Redo` take an optional `expected_op_id` (the entry the client shows; "per `op_id`" in 2.5 means this guard): another top fails with `ABORTED` / `UNDO_STACK_CHANGED`. With nothing to undo or redo the call succeeds without an entry, and its `request_id` is still recorded, so a late retry never acts on a newer op.
- The inverse is **guarded field restores**, in reverse event order: a field gets the event's `old` back only if it still holds the event's `new` (by value, so a field changed and changed back matches; string sets such as labels compare as sets; the status triple is one unit). Otherwise it keeps its value and is reported: `changed_since`, `item_deleted` or `invalid` (`not_undoable` is reserved, see below). Restores write the exact recorded values (the original `archived { at, reason }`, `deleted_at`), never `now`; the inverse of a capture is a soft delete at now.
- Undo and redo are **ops of their own** (kind `undo` / `redo`, actor `user`), linked to their entry and written like any edit: undo is itself recorded, and pushed to the tracker like any edit (inverting a Core change on a linked Item enqueues an outbox update, 5.6). Redo inverts the entry's latest undo under the same guard, and a later undo inverts that redo, so a partial undo gives a matching partial redo. An undo or redo that restores nothing **drops** the entry from both stacks and reports why.
- The parent rule (3.7) holds where the whole undo or redo leaves the Items, as for a batch, not after each step. A step that would leave a live Item under a deleted parent there is skipped as `invalid`: of the step that undeleted or moved the child and the one that deleted its parent, the later one.
- **Staleness** is restored, not refreshed: each op records, per touched Item, one `derived` event with the signals before and after. Undo restores a signal only together with the content it belongs to (also on an Item the undo itself deletes), and only where it still holds the after value; signals are never reported. An undo is not a user touch; `updated_at` is set to now.
- **Not undoable** (the op is skipped with a message): ops with an already-completed remote side effect that has no inverse — a successful promote/create, a pushed comment, a completed merge's Jira-side patches. If such effects are still queued (not yet pushed), undo cancels them instead.

**Derived staleness signals** (computed from events, cached on the Item): `last_user_touch_at`, `last_remote_change_at`, `status_entered_at`, `waiting_since`. These drive the anti-stagnation nudges in Section 6.

### 3.11 Suggestions

A **Suggestion** is a proposed change, never applied without your action:

```
Suggestion {
  id, item_ids[]                 // usually one; merges/relations involve several
  kind: refine | merge | split | prioritize | relate | fill_required | stale_review | comment_draft | sync_conflict | link_conflict | push_failed
  source: claude | rules | sync     // rules = produced by local deterministic rules (e.g. stale review without Claude, 6.5)
  rationale                      // one or two sentences, shown in the pane
  patches: [ Patch { id, op, group?, state: pending | applied | rejected | stale } ]
  state: pending | applied | partially_applied | rejected | expired | superseded
  prompt_version?                // Claude kinds (6.4)
  error?                         // push_failed: typed error detail, e.g. MissingFields, available transitions
  created_at
}
```

- A patch's **op** is one of: `set_field { item_id, field, expected_old, new }` · `set_status { item_id, expected_old, new }` (values are the derived 6-value status) · `add_relation { from, to, kind }` · `create_item { draft, parent? }` (split) · `merge_into { source, target }` · `add_comment_draft { item_id, body }` · `bump_touch { item_id }` (stale "keep": records an Event that sets `last_user_touch_at` = now) · `choose_transition { item_id, transition_id, name }`. Non-field ops carry their own precondition (e.g. both Items still exist and are not deleted).
- **`group`**: patches sharing a group id are **mutually exclusive alternatives** (at most one may be applied), e.g. a `stale_review`'s keep / someday / cancel / split options, or several `choose_transition` options. Patches without a group are independent.
- **Apply** (`ApplySuggestionPatches`) takes a list of patch ids plus optional **value overrides** per patch (the `e` edit in 2.6). It applies all still-valid selected patches in one op and returns which were skipped as stale. A suggestion becomes `applied` when all its patches are resolved and some were applied, `partially_applied` while some remain pending, `rejected` when all are rejected.
- **`superseded`** (Claude and rules kinds only): a new suggestion of the same (kind, item) — from `R` regenerate or a later job — replaces the pending one; patches already applied stay applied. **Sync kinds are never superseded**: each `sync_conflict` / `link_conflict` / `push_failed` stays until you apply or reject it, so a second conflict on the same Item never hides the first.
- **Lifecycle**: when an Item is deleted or merged away, its pending suggestions become `expired`. When it becomes `done`/`cancelled`, its pending Claude/rules suggestions expire; sync suggestions stay. Archiving changes nothing.
- Each **patch** is a checkbox in the Suggestions pane. You tick any subset and submit once; the accepted patches are applied as one `op_id` (undoable together).
- `expected_old` provides optimistic concurrency. If the field has changed since the suggestion was made, that patch is marked stale and cannot be applied. A suggestion whose patches are all stale becomes `expired`.
- Sync conflicts (5.6) and attach-time differences (3.3) use the same structure with `source: sync`. You review AI proposals and sync fallout in **one place, with one interaction**.
- Applying a patch goes through the same validation as a direct edit: the "assigned to someone else" warning (5.9) and the lossy-description confirmation (5.6) are shown by the client before apply, and the apply call carries the corresponding confirmation flags.

### 3.12 Versioning

Three independent version axes: the **API** is versioned by protobuf package (`todo.v1`, rules in 4.3); the **database** has a schema version with forward-only migrations (7.2); **exports** carry a `format_version` (7.4). At every layer, unknown fields are preserved, not dropped.

### 3.13 Worked example

1. **Capture.** In a terminal: `todo add "follow up w/ Dana re flaky login test"` → Item `01JB…` with `status = inbox`, title as typed, nothing else.
2. **Refine.** Claude produces a `refine` Suggestion with 5 patches: title → "Fix flaky login integration test", `item_type` → Bug, priority → medium, `waiting` → { on: Dana, since: today }, and a `relate` patch to the existing linked Item `PLAT-412` ("Auth test suite timeouts"). You tick all except priority and submit. One `op_id`, five events.
3. **Promote.** You promote the Item to Jira project PLAT → issue `PLAT-530` is created with title, description, type, labels and the relation as a Jira link. Core fields become Jira-owned. `waiting` remains a local overlay.
4. **Remote change.** A teammate moves PLAT-530 to "In Progress". Next pull: local status → `in_progress`; the `waiting` flag is untouched.
5. **Conflict.** You edit priority to high on a plane (queued in the outbox). Meanwhile a teammate sets it to highest in Jira. On reconnect Jira wins (priority = highest), and a `sync_conflict` Suggestion appears: "Your offline change high was overwritten by Jira (highest). Re-apply?"

## 4. Architecture

### 4.1 Shape

```
                ┌──────────────┐   ┌──────────────┐   ┌──────────────┐
                │  Ink TUI     │   │ `todo add …` │   │  MCP server  │   (future: web, mobile, …)
                │ (TypeScript) │   │  (Rust CLI)  │   │ (TypeScript) │
                └──────┬───────┘   └──────┬───────┘   └──────┬───────┘
                       │  gRPC over Unix domain socket (proto: todo.v1)
                ┌──────┴──────────────────┴──────────────────┴───────┐
                │                    todo daemon (Rust)              │
                │  API layer (tonic)                                 │
                │  Domain service ── single writer, events, revisions│
                │  Sync engine (per instance) ── Jira adapter        │
                │  Assistant (Claude) ── produces Suggestions        │
                │  Scheduler ── defer, recurrence, staleness, nudges │
                │  Store (SQLite)                                    │
                └────────────────────────────────────────────────────┘
```

**The daemon owns all state and all logic.** Clients are views and input devices: they hold no business rules beyond input validation and presentation. Every capability is a gRPC call, so any new client (a web UI, a phone app via a future network transport, a Raycast extension) gets the full feature set without touching the daemon. This is the main future-proofing decision.

### 4.2 Daemon lifecycle

- **One binary, `todo`** (Rust), with subcommands: `todo daemon` (run the server), `todo add …` (one-shot capture), `todo` with no arguments (launch the TUI), plus admin commands (`todo config`, `todo export`, `todo auth`).
- **Auto-spawn**: every client tries the socket first. If no daemon answers, it spawns `todo daemon --detach` and retries with a short backoff (budget ~2s). A lock file (exclusive `flock`) guarantees a single daemon per user.
- **Stays running** after clients disconnect, so background sync and scheduled jobs continue. An optional `todo daemon install` registers a launchd agent (macOS) / systemd user unit (Linux) so it starts at login. Not required.
- **Version handshake**: the first call from any client is `GetServerInfo(client_kind: interactive | oneshot | mcp, client_version, required_api_minor)` → daemon version, `api_minor` (an integer bumped with every additive change to `todo.v1`, and when an RPC that answered `UNIMPLEMENTED` starts working; 3 as of M2), and capability strings (e.g. `jira`, `assistant`). Interactive clients connected drive the faster pull cadence (5.4).
- **Upgrade restart**: if the daemon's `api_minor` is lower than the client requires, the daemon binary on disk is presumably newer (an upgrade happened). The client calls `Shutdown(restart = true)`; the daemon refuses if other interactive clients are connected (the client then shows "restart required; close other sessions"), otherwise it waits up to 10s for in-flight work (outbox ops `in_flight`, open Claude calls) and exits; the client re-spawns `todo daemon` from **its own install path**.
- **Socket**: `daemon.sock` in the runtime directory (7.1), mode `0600`; the gRPC `:authority` is `localhost`. No TCP listener in v1. A network transport (TLS + token) is a later, opt-in addition and must not change the API.

### 4.3 The gRPC contract

**Tooling**: protobuf definitions in `proto/`, managed by **Buf**. `buf lint` and `buf breaking` (against the main branch) run in CI, so wire-incompatible changes fail the build. Rust stubs are generated with `tonic`/`prost`. TypeScript stubs with `protobuf-es` and **Connect-ES** using the gRPC protocol over HTTP/2 on the Unix socket.

> Risk to retire early: confirm Connect-ES's Node transport can dial a Unix socket (via HTTP/2 `createConnection`) and interoperate with tonic. Fallback: `@grpc/grpc-js` (native `unix:` targets) + `ts-proto`. This choice is confined to the shared client package (4.6), so switching is cheap.

**Services** (package `todo.v1`), split by concern so they can evolve independently:

| Service | Responsibility |
|---|---|
| `SystemService` | Server info, health, shutdown/restart, config read + reload (the daemon **never writes** the TOML), export/import, backup |
| `ItemService` | Capture, `ParseCapture`, get, list/query, update (`FieldMask`), `SetStatus`, archive/unarchive, delete/undelete, relations, comments (local drafts), undo/redo, status counts (for `todo status`) |
| `ViewService` | Saved views: list, create, update, delete, reorder (4.8) |
| `PeopleService` | List/search (for `~` fuzzy match), get, update display name, merge duplicates |
| `RecurrenceService` | Series CRUD; completing an instance and generating the next one happen in **one op** |
| `ReviewService` | Weekly review sessions (start, resume, advance step), standup generation (works without Claude; Claude condenses when configured) |
| `SuggestionService` | List, apply (selected patches), reject, request generation |
| `SyncService` | Instances, link/promote/attach, sync now, sync status, auth setup |
| `AssistantService` | Claude-backed operations that aren't plain suggestions (Q&A, drafting) |
| `WatchService` | The change stream (below) |

**Contract rules** (binding on the API design phase):

1. **Additive evolution only** within `v1`: never renumber or reuse field numbers, and mark removed fields `reserved`. A breaking change means a new package `todo.v2`, served alongside `v1` for at least one release. Each additive change, and each stubbed RPC that starts working, bumps `api_minor` (4.2).
2. **Partial updates use `FieldMask`**, so old clients never wipe fields they don't know about. Clients must round-trip unknown fields.
3. **Enums** always have an `*_UNSPECIFIED = 0` value. Clients must handle unknown enum values gracefully (render as "unknown", never crash).
4. **Every mutating call** accepts a client-generated `request_id` (ULID; idempotency: a replay within 24h returns the original result) and returns the resulting `op_id` (for undo) and new `revision`.
   - **Bulk**: `ItemService.BatchMutate(mutations[])` applies a list of mutations (any of the single-item mutation messages) atomically as **one op** — what bulk actions on marked Items (2.5) use, so "Undid: mark done (3 items)" is one undo. Single-item RPCs are equivalent to a batch of one.
   - **Concurrency**: each mutation may carry `expected_revision` (the Item's `revision`, 3.1). If the Item changed since, the call fails with `ABORTED` and the client refreshes. The TUI always sends it for edits made from a rendered value; MCP and the CLI may omit it (last writer wins).
5. **Errors** use standard gRPC status codes with structured details (`google.rpc.ErrorInfo`, `BadRequest`). Domain-specific details such as `MissingFields` (5.7) are typed messages, never parsed from strings.
6. **Queries are server-side**: `ListItems` takes a structured filter (status, flags, dates, text, scope, archived, …), sort and page token. Clients don't filter large lists themselves. Saved views (Section 2) are stored filters.
7. Google's AIP guidelines (resource-oriented design, standard methods, pagination) are the default style reference where this doc is silent.

### 4.4 Change stream and revisions

- The store keeps a single monotonically increasing **revision**, incremented on every committed write (user, sync, suggestion, scheduler). Every read response (`GetItem`, `ListItems`, …) includes the revision it was read at; a paged `ListItems` pins all pages to the first page's revision.
- `WatchService.Subscribe(since_revision, topics[])` returns a server stream of `WatchMessage`, a oneof of:
  - **`Change { revision, topic, id, deleted: bool, resource: oneof { Item, Suggestion, View, Person, RecurrenceSeries, ReviewSession, ConfigSnapshot } }`** — durable, replayable. `topic` ∈ `item` (covers the Item aggregate incl. its comments, relations and pending-push state), `suggestion`, `view`, `person`, `series`, `review`, `config`. The full resource is included unless `deleted`.
  - **`Notice { kind, … }`** — ephemeral, **no revision**, not replayed: `sync_status` (5.11), `import_progress` (5.4), `assistant_job` (queued/running/done), `toast`.
  - **`ResyncRequired {}`** — sent once when `since_revision` is older than the replay window (7.2); the stream then continues from the current revision and the client must reload.
- Clients load data, remember its revision, subscribe from it, and resume from the last seen revision on reconnect. Because filtering is server-side, a client re-runs the current view's `ListItems` (debounced) when `item` changes arrive, or applies the change locally if it can evaluate membership cheaply.
- This one mechanism keeps the TUI live while sync and Claude work in the background, and it gives future clients the same live behavior for free.

### 4.5 Daemon internals (Rust)

Cargo workspace crates:

| Crate | Contents | Depends on |
|---|---|---|
| `todo-core` | Domain types, ownership rules, status mapping, suggestion patch application, recurrence, staleness computation. **No I/O.** | — |
| `todo-store` | SQLite persistence, migrations, revisions, events, outbox tables | core |
| `todo-sync` | Sync engine, adapter trait, fake adapter | core, store |
| `todo-jira` | Jira Cloud adapter, ADF ↔ Markdown | sync |
| `todo-assistant` | Claude providers, job queue, prompt construction, suggestion parsing | core, store |
| `todo-proto` | Generated code + conversions between proto and core types | core |
| `todo-daemon` | tonic services, task supervision, scheduler, watch fan-out | all above |
| `todo-cli` | The `todo` binary: subcommand dispatch, `add` + spool, admin commands, TUI/MCP launch, auto-spawn; `todo daemon` runs `todo-daemon` in-process | proto, daemon |

- **Runtime**: tokio. All writes go through one **domain service** that runs each mutation in a SQLite transaction, appends Events, bumps the revision, and publishes to the watch broadcaster after commit. A single writer keeps the ordering guarantees simple.
- **Background tasks** (one supervised task per sync instance, one assistant worker queue, one scheduler) are restarted on panic with backoff, and their health is reported via `SystemService`.
- The assistant calls the Claude API **from the daemon**, so the API key lives in one place and background work (staleness nudges, auto-refining new captures) runs without a client open. Section 6.

### 4.6 TypeScript side

pnpm workspace:

| Package | Contents |
|---|---|
| `packages/client` | Generated protobuf-es/Connect code, Unix-socket transport, auto-spawn + handshake, typed helpers, a small **live store** (normalized maps fed by `Subscribe`, exposed via `useSyncExternalStore`) |
| `tui` | The Ink app |
| `mcp` | The MCP server |

**TUI**: Ink (React for terminals, current major version), function components and hooks. State is kept light. Server data lives in the `packages/client` live store; UI-only state (focus, selection, open panes, checkbox selections in the Suggestions pane) is plain React state/context. No Redux-style global store. Keybindings live in a declarative keymap table, listed in the help overlay and overridable from config in v1 (8.2).

**MCP server**: a thin stdio MCP server (official TypeScript MCP SDK) that maps a curated set of tools 1:1 onto gRPC calls. Tools: capture an item, search/list items, get an item, update fields, set status, list pending suggestions (P2+). It contains no logic of its own. Launched as `todo mcp` and registered with Claude Code as a local stdio server (8.4).

- **Writes via MCP are allowed** when you use Claude Code interactively: you are directing it, so it is a user action (`created_via = mcp`, actor `user`). This is the one deliberate exception to "Claude only suggests" (Section 6).
- **`todo mcp --read-only`** exposes only read tools. The daemon's own `claude_code` provider (6.1) always uses this mode, so the background assistant can still only suggest.

**TUI launch**: `todo` with no arguments execs `node <tui.js>`, the bundled TUI, so the TUI owns the terminal and its exit status. The environment passes through, with `TODO_BIN` set to the running binary unless already set, so the TUI auto-spawns the same daemon. The bundle is the first that exists of: `$TODO_TUI_ENTRY` (when set and non-empty it must exist: a missing file is an error, not a fall-through); `<dir of the binary>/../share/todo-tui/tui.js` (the packaged layout, 8.4); `tui/dist/tui.js` in the source checkout the binary was built from (`target/debug/todo`, `cargo install --path crates/todo-cli`). With none, `todo` says to run `pnpm -C <checkout>/tui build` and exits 1; without `node` on `PATH` it says so and exits 1. A config key (8.2) can join the order once config exists. Packaging into a single executable is deferred to Section 8.

### 4.7 Repository layout

```
todo-tui/
├── proto/todo/v1/*.proto      # source of truth for the contract; buf.yaml, buf.gen.yaml
├── crates/                    # Cargo workspace (4.5)
├── packages/client/           # shared TS client
├── tui/                       # Ink app
├── mcp/                       # MCP server
├── docs/design.md             # this document
└── Cargo.toml, pnpm-workspace.yaml, buf.*.yaml
```

Generated code is not committed; CI and local builds run `buf generate`.

### 4.8 API modelling guidance

Binding conventions for the API design phase, so implementers don't diverge:

- **Presence**: nullable scalars use proto3 `optional`; nullable composites (`waiting`, `archived`, `remote_link`) use message presence. Empty `repeated` = empty set (no null/empty distinction; `components` is just empty when unlinked).
- **Enums**: `*_UNSPECIFIED = 0` is never a valid domain value. Domain "none" values are explicit (`PRIORITY_NONE`), and "no priority" is **always** `PRIORITY_NONE`, never `FieldValue.cleared`; `cleared` is only for fields without a domain "none" (dates, `waiting`, `parent`, overrides).
- **Types**: dates are `google.type.Date` interpreted in the user's local zone; instants are `google.protobuf.Timestamp`; durations are `google.protobuf.Duration`; `story_points` is `double`; `focus_week` is a string `YYYY-Www`; ids are ULID strings; remote extras are `map<string, google.protobuf.Value>`.
- **People**: `PersonRef { oneof { string person_id; string free_text } }` (free text only valid for `waiting.on`). Full `Person` records are fetched separately and streamed on the `person` topic.
- **Typed patch values**: `set_field.expected_old` / `new` use a `FieldValue` oneof covering every field type (string, markdown, enum values, date, person ref, string set, number, duration, …) plus `cleared`. No JSON strings. `field` is an enum `ItemField`, not a free-form path.
- **Update vs patch**: `UpdateItem` uses `FieldMask` over the same `ItemField` set. Applying a `set_field` patch is equivalent to an `UpdateItem` with precondition `expected_old`.
- **Filter language** (views, `ListItems`): `Filter { all_of: [Clause], any_of: [Clause], include_archived, include_done, include_deferred, include_deleted }`. Semantics: an Item matches if **every** `all_of` clause matches **and** (`any_of` is empty **or** at least one `any_of` clause matches), and it passes the implicit rules unless the matching `include_*` flag is true (all default false, 2.4).
  - A `Clause` is a typed predicate on one target: any stored field, any nested field (`waiting.on`, `waiting.follow_up_on`, `archived.reason`, `remote_link.instance_id`), or a derived field (`status`, `effective_priority`, `is_mine`, `is_overdue`, `has_conflict`, `scopes`, staleness signals). Operators by type: `in`, `=`, `!=`, `<`, `<=`, `>`, `>=`, `is_set`, `is_unset`, `contains_any`, `contains_all`, and `matches` (FTS text search). Date operands may be absolute or relative (`today`, `+Nd`, `this_week`; `current_week` for `focus_week`).
  - A Clause may instead be a nested group `{ all_of, any_of }` with the same semantics.
  - **Sort** is a list of `(key, direction)`; keys are any clause target. `none` priority sorts after `lowest`; unset dates sort last.
  - Default view filters: **Today** `any_of: [due_date <= today, status in [in_progress], {all_of: [waiting is_set, waiting.follow_up_on <= today]}, focus_week = current_week]`, sort `is_overdue desc, effective_priority desc, due_date asc`. **Archive** `include_archived, include_done, all_of: [archived is_set]`. The others are single `status`/`waiting`/`instance_id` clauses per 2.4.

## 5. Sync Engine

The sync engine keeps linked Items consistent with their tracker issues. It runs inside the daemon (Section 4), and it is the only component that talks to trackers.

### 5.1 Principles

1. **Ownership decides everything** (3.2). A pull overwrites Core fields of linked Items — **except fields with a pending local edit** (5.6) — and never touches Overlay fields. Local edits to Core fields are *requests* sent through the outbox.
2. **Local writes never block on the network.** Every edit commits locally first. Pushing is asynchronous and survives restarts.
3. **Nothing is silently lost.** Whenever the remote side wins over a local edit, the losing value becomes a Suggestion (`sync_conflict`, `link_conflict`, `push_failed`).
4. **Tracker-agnostic core, tracker-specific adapters.** The engine (cursors, outbox, conflict logic, scope tracking) is generic. Everything Jira-specific lives in the Jira adapter.

### 5.2 Tracker adapter interface

Adapters are compiled into the daemon and implement a Rust trait. This is a conceptual shape; exact signatures belong to the API phase:

| Operation | Purpose |
|---|---|
| `capabilities()` | Static flags: `transitions`, `story_points`, `sprints`, `components`, `rich_text_format`, `issue_links`, `parent_child`, `comments`, `create_metadata` |
| `discover(instance)` | Fetch and cache field metadata, priorities, issue types, statuses + categories, create-screen requirements |
| `pull_changes(scope, cursor)` | Stream normalized `RemoteIssue` snapshots changed since `cursor`; return a new cursor |
| `list_scope_ids(scope)` | Cheap full listing of remote ids in a scope (for reconciliation, 5.5) |
| `fetch(remote_id)` | → `Found(RemoteIssue) \| NotFound \| Forbidden` |
| `validate_create(draft)` / `create(draft)` | → `RemoteIssue` or `MissingFields[{ field, name, allowed_values? }]` |
| `update(remote_id, patches)` | Apply Core field patches |
| `transition(remote_id, target_category, preference?)` | Move between status categories |
| `add_comment(remote_id, body)` | |
| `to_remote_text(md)` / `from_remote_text(raw)` | Rich-text conversion, reporting lossiness |

`RemoteIssue` is a **normalized** snapshot: Core fields in local vocabulary, plus `extras` (raw custom fields), plus `remote_updated_at`. Adapters must not leak tracker-native shapes into the engine.

**Capabilities drive behavior elsewhere.** The UI hides unsupported fields, Claude does not suggest story points for a tracker without them, and transitions fall back to plain status setting for trackers that lack workflows.

Interface stability: the trait is versioned, and adding a capability flag or optional operation must not break existing adapters. If out-of-process adapters are ever wanted, this trait is the seam to expose over gRPC, but that is explicitly not v1.

### 5.3 Instances, scopes, cursors

- An **instance** is one configured tracker connection (e.g. `work-jira`). Multiple instances, including different tracker kinds, are supported by the model from day one; v1 ships and tests one Jira instance.
- Each instance has one or more named **scopes**, each a tracker-native query. v1 has one default scope: `assignee = currentUser() AND resolution = Unresolved`. Scopes are config, not code, so "reported by me" or "team sprint" are added later without design changes.
- Each linked Item records which scopes it currently matches.
- **Tracked set**: every linked, non-deleted Item — in scope or not, pinned or not — is kept in sync by id. Scope queries discover and import issues; the tracked set keeps known issues fresh (5.4 step 2b).
- An unresolved, **unpinned** Item in no scope is archived with `left_scope` (3.7) and unarchived automatically if it matches again. **Pinned** Items (created by promote or attach, 3.3) are never archived for leaving scope, so promoting an issue that defaults to another assignee, or attaching a teammate's issue, keeps it in your views.
- Each (instance, scope) keeps a **cursor**: the highest `remote_updated_at` it has seen. Pulls query `updated >= cursor − overlap` (Jira JQL time has minute granularity; overlap ≈ 2 minutes), and the result is deduplicated by comparing against stored snapshots.

### 5.4 The pull loop

Per instance, one task:

1. **Cadence**: every ~60s while any interactive client (TUI) is connected; every ~10 min otherwise; immediately on a manual "sync now"; and immediately for a single item after a push to it. All intervals are configurable.
2. **Fetch changed issues**: (a) per scope, `updated >= cursor − overlap`, page by page; (b) for the tracked set *outside* every scope, the same `updated >=` condition restricted to their ids (batched `id in (…)` queries, ≤ 100 ids per query).
3. **For each `RemoteIssue`**: find the Item by `(instance_id, remote_id)`. If it is deleted (tombstoned), skip it. Otherwise create an Item (new import, `created_via = remote_import`) or diff against the stored **remote snapshot**.
4. **Apply the diff** to Core fields, emitting Events with `actor = sync`, **except** for fields that have a pending outbox patch: for those, the pending local value stays displayed and the conflict rule in 5.6 runs immediately. Optionally (if the adapter supports it) attribute changes to a remote user from the issue changelog for display ("Alex changed priority").
5. **Store the new snapshot**: the last normalized `RemoteIssue`, including the raw rich-text description and the most recent comments (default 50). This snapshot is the base for diffing the *next* pull; conflict detection uses the per-patch base values stored in the outbox (5.6).
6. Advance the cursor only after the page is committed.

**New imports** get a `todo_refinement` matching their category: To Do → `next` (3.4). The first sync runs as a bulk import that streams progress to clients.

### 5.5 Reconciliation (deletions, scope exits, moves)

Change queries do not report issues that were deleted or that left the query. Periodically (default hourly, plus on startup) and per scope:

1. `list_scope_ids(scope)` → set R. Linked Items marked in-scope → set L. Additionally, for the tracked set *outside* every scope (5.3), list which ids still exist via batched `id in (…)` queries (ids only); every id not returned goes to step 2's `fetch` as well.
2. For each id in L − R: `fetch(id)`.
   - `Found` → **apply the fetched snapshot** (as in 5.4 steps 4–5, so a resolution in Jira becomes `done`/`cancelled`), remove scope membership, and — only if the issue is unresolved, unpinned and in no scope — archive `left_scope`.
   - `NotFound` or `Forbidden` → link `broken`, archive `remote_unavailable` (the detail pane says which), surfaced in sync status.
3. For each id in R − L: if it has an Item, add scope membership (unarchiving a `left_scope` Item); if it is deleted, skip; otherwise import it.

Issue moves (key change) need no special handling, because identity is `remote_id`. The stored `key` is simply updated.

### 5.6 The outbox (pushes)

Every local change to a linked Item's Core fields, plus every promote, comment and transition, becomes a durable **outbox operation**:

```
OutboxOp {
  id, item_id, instance_id, op_id     // op_id ties back to the user action / Events
  kind: create | update | transition | comment | link
  payload                             // for update: [{ field, new, base }] — `base` is the field's remote value
                                      //   in the snapshot the edit was made against; for transition: target + base category; …
  state: pending | in_flight | done | failed | cancelled
  attempts, next_attempt_at, last_error
}
```

- **Ordering**: ops run in order (FIFO) for a given Item; different Items run in parallel up to a small limit (default 2–4 concurrent requests per instance).
- **Coalescing**: pending `update` ops on the same Item and field collapse to the latest value, keeping the **original** `base`.
- **Retry**: transient errors (network, 5xx, 429) retry with exponential backoff and honor `Retry-After`. Permanent errors (400 validation, 403, invalid transition) mark the op `failed` and create a `push_failed` Suggestion containing the error and the rejected values.
- **Optimistic display**: the local Item shows the pending value immediately, with a "pending push" marker (exposed via the API so clients can render it).

#### Conflict rule (at pull time and at push time)

For each pending patch, compare the current remote value with the patch's stored `base`. This runs whenever a pull sees the field (5.4 step 4) and again just before pushing (the engine fetches the issue first):

- Remote value **equals `base`** → no conflict; keep pending / push.
- Remote value **equals the patch's `new`** → no conflict; the patch is satisfied and dropped.
- Otherwise → **Jira wins**. Drop that patch, set the local value to Jira's, and create a `sync_conflict` Suggestion (one unchecked `set_field` patch) offering to re-apply the local value. Other patches in the op are unaffected.
- **Transitions** follow the same rule on `status_category`: if the remote category differs from the op's base category and from its target, the transition is dropped and becomes a `sync_conflict`.

Jira has no conditional update (no `If-Match`), so a small race window remains between the read and the write. That is acceptable for a personal tool, and it is documented, not engineered away.

#### Description safety

The description is pushed **only if it was edited locally** (`description_dirty`). If the remote description contains rich-text nodes the converter cannot represent, the Item is flagged `description_lossy`. Editing it then requires an explicit confirmation in the UI ("formatting X will be lost in Jira"), and the backend refuses to push a lossy description without that confirmation flag.

### 5.7 Promote (create)

1. **Validate first, even offline.** Using cached create metadata (5.8), `validate_create` checks the target project and issue type for required fields. If any are missing, promotion fails **immediately** with a structured `MissingFields` list (field id, display name, allowed values if enumerable). The Item stays local.
2. The client shows the missing fields. **If Claude is configured**, the client may request a Claude Suggestion that proposes values for exactly those fields, constrained to the allowed values. You tick and submit, then retry promote.
3. Once valid, a `create` op is queued and the Item gets a `RemoteLink` with `link_state = pending`, `pinned = true`.
   - **While pending**, Core edits are folded into the queued create's payload (not separate updates); the Item can be deleted (cancels the create if not in flight) but not attached or promoted again.
   - **Issue type**: the Item's `item_type` if it is valid for the target project, else `promote_default.issue_type`. A child whose parent is linked is created with the instance's `subtask_issue_type` (or as a child via the parent field for next-gen projects, per discovery).
   - The create request sends every Core field that is set locally (including `assignee` if set; if unset, Jira's default applies).
   - On success, `link_state = ok`, Core ownership flips to remote, and the returned issue becomes the first snapshot. Any Core value that was **sent but came back different** (e.g. Jira overrode priority) is offered back as a `link_conflict` Suggestion, exactly as for attach (3.3). Fields that were unset locally and filled by Jira defaults are simply adopted.
4. **Creates are not idempotent in Jira.** If a create times out with an unknown outcome, the engine must **not** blindly retry. It first searches for an issue it may have created (recent issues by the current user with a matching summary, and an issue property `todo-tui.local_id` set at creation if the instance allows it). It retries only if none is found. See Open Questions.
5. If the remote metadata has changed (a new required field) and the create fails server-side, it becomes a `push_failed` Suggestion listing the missing fields. The flow returns to step 2.

### 5.8 Field mapping and discovery

Per instance, `discover` runs on setup, daily, and on demand. It caches:

- Field catalogue (ids ↔ names) — used to auto-detect the story points field, sprint field, epic/parent field.
- Priority names → mapped onto the five non-`none` local levels by rank (Jira returns them ordered): with exactly 5, one-to-one; with fewer, spread across the scale; with more, the extras collapse to the nearest level. Local → Jira picks the Jira priority mapped to that level (first by rank on ties). Override with `priority_map` in instance config. `none` maps to "unset" only if the field is optional.
- Resolutions: which resolution names mean `cancelled` (`cancelled_resolutions`, default `["Won't Do", "Won't Fix", "Duplicate", "Cannot Reproduce", "Declined"]`); every other resolution means `done`.
- The authenticated user's account id (for `is_mine`, 5.9).
- Issue types, statuses, and status → category mapping.
- Create-screen metadata per (project, issue type): required fields and allowed values.
- Transitions are **not** cached globally. They are fetched per issue when needed, because they depend on the workflow and current status.

**Instance config** (format in 8.2) holds: base URL, account email, credential reference, scopes, default promote target (project + issue type), priority mapping overrides, custom-field overrides, and transition preferences per (project, target category).

### 5.9 Editing Items you don't own

All linked Items are editable locally. If the Item's assignee is not the authenticated user, the API marks it `is_mine = false`. Clients must warn before submitting a Core edit ("PLAT-412 is assigned to Alex — push change to Jira anyway?"). The edit then follows the normal outbox path.

### 5.10 Credentials

A `CredentialStore` abstraction with backends in priority order: macOS Keychain → Linux Secret Service → environment variable → permission-checked file. Instance config stores only a credential *reference*. v1 auth for Jira Cloud is email + API token (basic auth). OAuth 2.0 (3LO) is a later addition behind the same abstraction.

### 5.11 Offline behaviour and sync status

- Network failure moves an instance into `offline` state. The pull loop pauses with backoff probing, and the outbox keeps accepting ops. On reconnect: pull first (so conflict checks use fresh data), then drain the outbox.
- Each instance exposes a **sync status** stream: `state (ok | syncing | offline | auth_error | degraded)`, `last_successful_pull_at`, `pending_ops`, `failed_ops`, `last_error`. Clients render it as a compact indicator.

### 5.12 Failure modes

| Situation | Behaviour |
|---|---|
| API token expired / revoked | Instance → `auth_error`; pulls and pushes pause; outbox retained; prominent status in UI |
| Permission lost on an issue | Link `broken`; archived `remote_unavailable`; content kept |
| Issue deleted remotely | Link `broken`; archived `remote_unavailable`; content kept |
| Issue moved to another project | `key` updated; nothing else |
| Transition not available / needs fields | Op `failed` → `push_failed` Suggestion with a `choose_transition` group and/or missing fields |
| Required field added after creation | Update may fail validation → `push_failed` with the field list |
| 429 rate limit | Honour `Retry-After`; reduce concurrency for the instance temporarily |
| Lossy description edited | Push refused without explicit confirmation flag |
| Ambiguous create (timeout) | Reconcile-before-retry (5.7 step 4) |

### 5.13 Testing strategy

- An **in-memory fake adapter** implementing the trait, with controllable time, failures and remote edits. Most engine tests (conflicts, outbox ordering, reconciliation, tombstones) run against it.
- **Recorded HTTP fixtures** of real Jira Cloud responses for adapter tests (mapping, ADF conversion, pagination, error shapes).
- An optional, manually triggered live test suite against a sandbox Jira project.

> Implementation note for Jira Cloud (verify at build time): use REST API v3; issue search via the enhanced JQL search endpoint with token pagination; per-issue changelog for attribution; per-issue transitions endpoint; create-metadata per project/issue type. Rate limiting is signalled by HTTP 429 with `Retry-After`. JQL date literals are interpreted in the **Jira user's profile time zone** at minute precision, so cursors must be stored as UTC instants and converted to that zone (fetched with the account during discovery) when building queries.

## 6. Claude Integration

Claude is **optional**: with no provider configured, every feature in this section is absent, and the rest of the system is unaffected. When present, the daemon's assistant **only proposes**. Everything it produces is a Suggestion (3.11) or read-only text. It never writes to Items, Jira, or comments directly. (Separately, *you* may direct Claude Code to edit Items through the MCP server; that is a user action, see 4.6.)

### 6.1 Providers

The assistant talks to Claude through a `ClaudeProvider` abstraction in `todo-assistant` with one operation: *run a task with this system prompt, context, optional read-only tools, and output schema → validated JSON (or text) + token usage*.

| Provider | Auth | Billing | Notes |
|---|---|---|---|
| `api` | `ANTHROPIC_API_KEY`, or an `ant auth login` OAuth profile | Claude API (Console) | Raw HTTP to `POST /v1/messages` (no official Rust SDK). Supports structured outputs, prompt caching, model per task. **Default.** |
| `claude_code` | Whatever the user's installed Claude Code is logged into, including a Pro/Max subscription | User's Claude plan | Daemon shells out to the user's `claude` CLI in headless/print mode with JSON output. Read-only tools are provided by pointing it at `todo mcp --read-only` (4.6). Slower per call; batching (6.3) matters more. |

Claims to verify at build time: `ant auth login` OAuth profiles work for direct HTTP calls; structured outputs via `output_config.format`; current Claude Code headless flags. Rules: the app never reads, copies or refreshes Claude Code's credentials itself. It only invokes the user's own `claude` binary. Before open-sourcing, confirm that this provider is consistent with Anthropic's current terms for third-party tools.

### 6.2 Tasks

Every Claude feature is a **task kind**, with its own prompt template, output schema, default model and trigger. Model and effort are per-kind config, so you can tune cost without code changes.

| Task | What it produces | Default trigger | Default model |
|---|---|---|---|
| `refine` | Clean title, description, type, priority, due date, labels, waiting-on inferred from prose; **or** a `merge` suggestion if the capture duplicates an existing Item | Auto on capture (config: `auto` \| `on_request`) | Haiku 5.5, low effort |
| `relate` | Duplicate / relates-to / blocks proposals between existing Items | Part of `refine`; on request | Haiku 5.5 |
| `prioritize` | Priority / due date proposals with rationale | On request; part of weekly review | Haiku 5.5 |
| `split` | Child Items (local, 3.6) for a large Item | On request | Sonnet 5.5 |
| `fill_required` | Values for missing required Jira fields, constrained to allowed values (5.7) | On promote failure, if configured | Haiku 5.5 |
| `stale_review` | Per-stale-Item action proposals (6.5) | Scheduled, weekly | Sonnet 5.5 |
| `comment_draft` | A Jira comment / status update for an Item | On request | Sonnet 5.5 |
| `standup` | Personal standup text (6.6) | On request | Sonnet 5.5 |
| `ask` | Answer to a free-form question, with Item references (6.7) | On request | Opus 5.5 |

Model IDs are config values (`claude-haiku-5-5`, `claude-sonnet-5-5`, `claude-opus-5-5` at time of writing). Defaults favor cheap models for high-frequency work.

### 6.3 Pipeline

```
trigger ──► job queue ──► context builder ──► provider ──► validate ──► Suggestion(s) ──► watch stream ──► TUI
```

1. **Job queue** (daemon, persistent): `{ kind, item_ids, trigger: auto | user | scheduled, created_at }`. User-triggered jobs jump ahead of background ones. Duplicate jobs for the same (kind, item) collapse. Jobs wait while the provider is unreachable, so capture works offline and refinement catches up later.
2. **Debounce and batch**: auto-`refine` waits a short window (default 30s) after capture, so a quick edit isn't wasted, and batches all captures in that window into **one** call.
3. **Context builder** assembles, in cache-friendly order (6.4):
   - the stable system prompt for the kind and the output schema;
   - **constraints** from tracker metadata: allowed priorities, issue types, labels and components in use, required fields with allowed values;
   - **your preferences**: style notes from config, plus recent accepted/rejected examples (6.8);
   - **candidates**: for refine/relate, the top ~20 existing Items from local SQLite FTS5 search on the capture text (titles, keys, short descriptions only), so duplicate detection never sends your whole database;
   - the target Item(s) themselves.
4. **Provider call** with structured output (JSON schema per kind; on `api`, via `output_config.format`).
5. **Validate**: parse against the schema. Drop any patch whose field or value is invalid for the target: unknown priority, disallowed component, a Core field on a tracker without that capability. Set each patch's `expected_old` to the Item's value **as sent in the context**, so anything you changed meanwhile makes that patch stale (3.11).
6. **Emit** Suggestions. They reach the TUI through the normal change stream.
7. **Record usage** (input/output/cached tokens, model, latency) per job, viewable with `todo usage`.

No hard spending cap. Cost stays low through: cheap default models, batching, debounce, FTS-limited context, prompt caching, and **skip-if-unchanged** (a job whose inputs hash to the same value as a previous job is not re-run).

### 6.4 Prompt structure and caching

Each request is ordered most-stable first: system prompt (fixed per kind and version) → schema → constraints (change when tracker metadata changes) → preference examples (refreshed at most daily) → candidates and target (change every call). Cache breakpoints go after the constraints and after the preferences. Caching only applies above a model-dependent minimum prefix size, so small prompts may simply not cache; that is acceptable. Prompt templates are versioned files in the repo, and the version is recorded on each Suggestion for debugging.

### 6.5 Anti-stagnation and the weekly review

**Stale rules** (defaults; all configurable). Computed by the scheduler from the derived signals in 3.10:

| Condition | Default threshold |
|---|---|
| `next`, untouched by you | 14 days |
| `in_progress`, no local or remote change | 7 days |
| `waiting`, since-date passed without follow-up | 7 days, or `follow_up_on` passed |
| `someday`, untouched | 90 days |
| `inbox`, unprocessed | 3 days |
| Linked, no remote activity | 30 days |

**Weekly review session**: a guided TUI flow at the start of the week (default Monday; the scheduler pre-generates everything in the background beforehand so it opens instantly):

1. **Inbox to zero**: each inbox Item with its `refine` Suggestion ready to apply.
2. **Stale Items**: each with a `stale_review` proposal: one of *keep (bump touch date)*, *move to someday*, *cancel*, *split*, *re-prioritize*, *follow up with X* (which offers a `comment_draft` for Jira Items).
3. **Waiting on others**: who you're waiting on and how long; nudge or clear.
4. **Coming up**: due this week, deferrals expiring.
5. **Focus**: pick this week's focus Items (sets the `focus_week` Overlay field, 3.2; focus Items appear in Today).

Every step uses the standard Suggestions checkbox interaction. The scheduler always creates a baseline `stale_review` suggestion per stale Item with `source: rules`: one alternative group of *keep* / *move to someday* / *cancel*, rationale = the rule that fired. With Claude configured, the Claude `stale_review` job supersedes it with a richer version (its own rationale plus *split*, *re-prioritize*, *follow up* options). So the review works without Claude, just with fewer options.

### 6.6 Personal standup

`todo standup` (and a TUI view) produces a short text: *done since last standup*, *in progress / planned today*, *blocked / waiting on*, built from Events since the last standup (the time of the last `todo standup` is stored; default for the first run: the previous working day). Without Claude it is a plain list; with Claude it is condensed into readable sentences. Output can be copied to the clipboard. It is never posted anywhere automatically.

### 6.7 Ask

The **Ask bar** (`?` in the TUI, `todo ask "…"` on the CLI) takes one question ("what am I blocked on?", "what's due before Friday?", "anything about the auth migration?") and returns one answer, rendered in a transient panel with selectable Item references. It is not a persistent chat. Claude gets **read-only tools** (search Items, get Item, list recent Events). If the answer implies changes ("you should cancel these three"), they are emitted as Suggestions of the matching kind (e.g. `prioritize`, `stale_review`), never applied.

### 6.8 Learning from your choices

Every apply or reject records, per patch: kind, field, proposed value, accepted or rejected, and your edited value if you changed it before applying. The context builder includes a small sample of recent decisions for the same kind as examples. Config also has a free-text **style notes** field ("titles imperative, under 60 chars; never set priority above high"), included in every prompt. No model fine-tuning; all learning is in-context.

### 6.9 Natural-language capture

Two layers, both on the same capture path (`todo add`, TUI quick-add, MCP):

1. **Local parser (no Claude, instant, deterministic)**: explicit inline tokens you typed are applied directly, as edits not suggestions (syntax in 2.3): priority, due date, labels, waiting-on, status.
2. **Claude `refine`**: infers fields from the *prose* ("ping Dana fri about login test" → due Friday, waiting on Dana). Because these are inferences, they arrive as Suggestions.

### 6.10 Data sent to Claude

What is sent: Item Core and Overlay fields of the Items in context, minus `private_notes` (excluded by default; config can include them). A global switch (omit `assistant.provider` to disable) and a per-instance toggle (`assistant_enabled`, 8.2) exist even though v1 assumes sending is fine, so a future corporate policy needs no redesign.

## 7. Storage, Offline & Export

### 7.1 Storage engine

- **SQLite** in WAL mode, one database file, owned exclusively by the daemon. No client ever opens it.
- Rust access via `rusqlite` with a **dedicated writer thread** (the single writer from 4.5) and a small read connection pool, bridged to tokio. FTS5 is required (search, 6.3 candidates).
- Locations:

| What | macOS | Linux |
|---|---|---|
| Config | `$XDG_CONFIG_HOME/todo-tui/config.toml`, default `~/.config/todo-tui/config.toml` | same |
| Data: database, backups, spool | `~/Library/Application Support/todo-tui/` | `$XDG_DATA_HOME/todo-tui/` (default `~/.local/share/todo-tui/`) |
| Logs | `~/Library/Logs/todo-tui/` | `$XDG_STATE_HOME/todo-tui/` (default `~/.local/state/todo-tui/`) |
| Runtime: socket, lock | `$TMPDIR/todo-tui/` (per-user on macOS) | `$XDG_RUNTIME_DIR/todo-tui/`, fallback `/tmp/todo-tui-$UID/` (mode `0700`) |

- The runtime directory is chosen to keep the socket path well under the ~104-byte Unix socket limit. If the resolved socket path would exceed 100 bytes, the daemon falls back to `/tmp/todo-tui-$UID/`.
- `TODO_TUI_HOME=<dir>` overrides everything with a fixed layout: `<dir>/config.toml`, `<dir>/data/`, `<dir>/logs/`, and the runtime dir `<dir>/run/` (same 100-byte fallback rule). Used for tests and multiple profiles.

### 7.2 Logical schema

Conceptual tables. Exact columns belong to the implementation phase, but each maps to a concept in Section 3 or 5:

`items` (incl. soft-delete / tombstone columns, 3.7) · `remote_links` · `remote_snapshots` · `remote_extras` · `relations` · `people` (+ `person_identities`) · `comments` · `recurrence_series` · `events` · `ops` (op_id, actor, undo/redo state) · `suggestions` · `suggestion_patches` · `suggestion_decisions` (6.8) · `outbox` · `sync_instances` (cursors, state, account id, time zone) · `scope_membership` · `tracker_metadata` (cached discovery, 5.8) · `views` (4.8) · `review_sessions` (2.7) · `assistant_jobs` · `assistant_usage` · `request_ids` (idempotency, 24h) · `spool_imports` (imported spool ids) · `kv` (e.g. last standup time) · `changes` (watch replay, 4.4) · `meta` (schema version, current revision) · `items_fts` (FTS5 over title, description, key, labels).

- **Migrations** are embedded in the binary, forward-only, and run at daemon start inside a transaction, after an automatic pre-migration backup. A daemon refuses to open a database whose schema version is newer than it understands. Real databases exist since M1, so from `0002` on every schema change is a **new** migration, and the statements of a migration that databases have applied are never edited (this replaces "edit `0001` until the first release").
- **`changes` retention**: last 30 days or 50k rows, whichever is larger. Clients older than that get `RESYNC_REQUIRED` (4.4).
- **`events` retention**: kept indefinitely (personal scale is small). A compaction policy can be added later without changing semantics.

### 7.3 Offline

The system is **offline-first**: all reads and writes are local, and only two subsystems use the network.

| Works offline | Degrades offline |
|---|---|
| Capture (all paths), all views, search, editing, undo, export, backups, local stale rules, review flow without proposals | Jira pull/push (outbox queues, 5.11); Claude jobs (queue waits, 6.3); `o` (open in browser) |

The status bar makes offline state obvious. Nothing prompts or blocks because of it.

### 7.4 Export and import (machine-readable)

- `todo export [--out PATH]` (also the `SystemService.Export` server stream) writes **JSON Lines**:
  - line 1: header `{ "format": "todo-tui-export", "format_version": 1, "schema_version": N, "exported_at": "…", "daemon_version": "…" }`
  - each following line: `{ "type": "<record type>", "data": { … } }`, one per record, in dependency order: people → series → items (incl. deleted) → remote_links → remote_snapshots → remote_extras → scope_membership → relations → comments → views → sync_instances (cursors) → outbox (non-`done`) → ops → events → suggestions (+ patches) → suggestion_decisions → review_sessions → kv.
- **Record shapes are protobuf messages in their canonical proto3 JSON mapping.** Records that no RPC exposes (snapshots, outbox ops, cursors, …) get messages in a dedicated `todo.v1.export` package. The `.proto` files are the export schema, with no second schema to maintain, and follow the same additive-evolution rules (4.3).
- **Excluded** (caches, derived or ephemeral): `tracker_metadata`, `changes`, `items_fts`, `assistant_jobs`, `assistant_usage`, `request_ids`, `spool_imports`, `done` outbox ops. `--no-history` omits `events` and `ops`. **Never included**: credentials or API keys.
- Snapshots, cursors and pending outbox ops are included so a restore resumes sync without a full re-import and without losing conflict detection.
- `todo import PATH` restores an export into an **empty** database (full-fidelity round trip: export → import → export yields identical record content). Merging into a non-empty database is out of scope for v1 (Section 9).

### 7.5 Backups

Automatic backups exist **only together with automatic cleanup**:

```toml
[backup]
enabled = true        # default: true
interval = "daily"    # default: "daily"
retain_days = 14      # default: 14; must be ≥ 1
```

With no `[backup]` section the defaults apply (daily, 14 days).

- Implemented with SQLite `VACUUM INTO` to `backups/todo-YYYYMMDD-HHMM.db` (consistent snapshot, no daemon pause).
- Each backup run deletes backups older than `retain_days`, including pre-migration backups. If `retain_days` is set to an invalid value (< 1, not a number), automatic backups are disabled and a warning is shown (the daemon still starts, per 8.2's warnings-not-errors rule). The daemon never accumulates backups without bound.
- `todo backup now` and `todo restore PATH` (restore stops the daemon, swaps the file, restarts).

## 8. Extensibility & Open-Source Readiness

### 8.1 Extension points

| To add… | You touch | Not touched |
|---|---|---|
| A new client (web, Raycast, Shortcuts, mobile) | A gRPC client of `todo.v1` | Daemon |
| A new tracker (GitHub, Azure DevOps) | A new `todo-<tracker>` crate implementing the adapter trait (5.2) + its config section | Engine, store, clients (capabilities adapt the UI) |
| A new Claude feature | A task kind: prompt template, output schema, trigger, default model (6.2) | Pipeline, Suggestions UI |
| A new Claude backend | A `ClaudeProvider` impl (6.1) | Tasks |
| A new view | `:view new` / `ViewService` (stored in the database) | Code |
| Different keys | Keymap config | Code |
| A new credential store | A `CredentialStore` impl (5.10) | Adapters |
| A new field | Core: proto + core + adapter mapping. Overlay: proto + core only | Sync engine logic (ownership comes from the layer) |

**Adding a tracker, checklist**: implement the trait; declare capabilities; map statuses to the three categories and priorities to the local scale; implement rich-text conversion (or plain text); record HTTP fixtures; pass the shared adapter conformance test suite that runs against every adapter (pull/push/create/transition/reconcile scenarios, built on the fake adapter's scenarios).

### 8.2 Configuration

TOML at the path in 7.1. Top-level `config_version`. Unknown keys produce warnings, not errors. `todo config check` validates. **Secrets never appear in config**, only credential references (5.10). Illustrative shape:

```toml
config_version = 1

[ui]
review_day = "monday"
archive_done_after_days = 7
# views are stored in the database (2.4), not here

[keymap]
# "p" = "set_priority"   # overrides only

[backup]
enabled = true
interval = "daily"
retain_days = 14

[[instances]]
id = "work-jira"
kind = "jira"
base_url = "https://example.atlassian.net"
email = "me@example.com"
credential = "keychain:todo-tui/work-jira"
promote_default = { project = "PLAT", issue_type = "Task" }
subtask_issue_type = "Sub-task"
# priority_map = { highest = "Blocker", high = "Critical" }   # overrides only
# cancelled_resolutions = ["Won't Do", "Duplicate"]            # overrides default (5.8)
scopes = [{ name = "mine", query = "assignee = currentUser() AND resolution = Unresolved" }]
assistant_enabled = true

[assistant]
provider = "api"            # "api" | "claude_code" | omit to disable
auto_refine = "auto"        # "auto" | "on_request"
style_notes = ""
include_private_notes = false
[assistant.models]
refine = "claude-haiku-5-5"
stale_review = "claude-sonnet-5-5"
ask = "claude-opus-5-5"

[stale]
next_days = 14
in_progress_days = 7
```

### 8.3 Open-source readiness

- **Name** `todo-tui`. **License**: MIT OR Apache-2.0 (dual).
- **No work data in the repo**: Jira URLs, project keys, people and fixtures must be synthetic. Recorded HTTP fixtures are scrubbed before commit (a CI check rejects fixtures that contain non-placeholder domains or emails).
- **No telemetry.** Claude usage stats (6.3) are local only.
- **CI**: `buf lint` + `buf breaking`; `cargo fmt --check`, `clippy -D warnings`, `cargo test`; `pnpm typecheck`, `lint`, `test`; adapter conformance suite; export round-trip test.
- **Docs**: this design doc plus `docs/adr/` (one short Architecture Decision Record per significant change to it), `CONTRIBUTING.md`, and an "adding a tracker" guide derived from 8.1.

### 8.4 Distribution

- **Platforms**: macOS (primary) and Linux. Windows is not supported in v1.
- **Homebrew** (tap first) is the primary install. It installs the `todo` binary plus the bundled TUI and MCP JavaScript (each a single esbuild bundle), with Node as a runtime dependency.
- `cargo install` is for contributors (Rust part only); the TUI then runs from the repo with `pnpm`.
- **MCP registration** is one command: `claude mcp add todo -- todo mcp`. `todo mcp` execs the bundled TypeScript MCP server (still a separate thin process, 4.6).
- Removing the Node runtime dependency (single-executable packaging) is a later option, tracked in Section 9.

## 9. Open Questions & Phased Roadmap

### 9.1 Roadmap

Each phase ends usable. Later phases must not require reworking earlier contracts, and the contract rules in 4.3 apply from P0.

| Phase | Scope | Exit criteria |
|---|---|---|
| **P0 — Spike** | Repo skeleton (4.7 layout), Buf setup, a minimal `todo.v1` (`GetServerInfo`, `CaptureItem`, `ListItems`, `Subscribe`) using the **real** `Item` message shape from 3.2/4.8 (it can't be renumbered later), tonic daemon on a Unix socket with SQLite + revisions + `changes`, Connect-ES client in Node, Ink list that updates live | TS client calls tonic over UDS incl. a server stream; auto-spawn + lock works. Fallback decision (4.3) made |
| **P1 — Local core** | Full domain model (Section 3, minus remote parts, Suggestions and comment drafts, which move to P2 where they can be pushed), store + migrations, capture parser + `ParseCapture`, `todo add` + spool, TUI layout/views/keymap/detail/undo/bulk, recurrence, archive/delete/undelete, export/import, backups, `todo status`, MCP server (incl. `--read-only`). **Services**: `SystemService`, `ItemService` (incl. `BatchMutate`), `ViewService`, `PeopleService`, `RecurrenceService`, `WatchService` complete for local data | Daily-drivable as a local-only todo app |
| **P2 — Jira sync** | Adapter trait + fake adapter + conformance suite, Jira adapter, discovery, pull loop + tracked set, reconciliation, outbox, promote/attach, conflicts, comment drafts and their push, **Suggestion model + pane** (for `sync_conflict` / `link_conflict` / `push_failed`), credentials, sync status. **Services**: `SyncService`, `SuggestionService` | Your assigned Jira issues stay in sync both ways on a work instance; offline edits recover correctly |
| **P3 — Claude core** | Providers (`api`, `claude_code`), job queue, context builder, `refine` (incl. merge), `relate`, `prioritize`, `fill_required`, learning from decisions, NL inference, usage tracking. **Services**: `AssistantService` (request jobs, usage) | Captures arrive refined; duplicates are caught |
| **P4 — Review & Claude workflows** | Stale rules + `stale_review` (rules and Claude), weekly review mode, standup, Ask bar, `split`, `comment_draft`. **Services**: `ReviewService` | Monday review is the routine |
| **P5 — Open source** | Packaging (Homebrew), docs, fixture scrubbing, a **second adapter** (GitHub Issues) to prove the adapter abstraction | Public release |

### 9.2 Open questions

| # | Question | Default until decided | Decide by |
|---|---|---|---|
| 1 | Can Connect-ES dial a Unix socket and interoperate with tonic (incl. streams)? | Fallback: `@grpc/grpc-js` + `ts-proto` | P0 |
| 2 | How to safely reconcile an ambiguous Jira create (issue property vs. heuristic search)? | Heuristic: recent issues by me with an identical summary in the last 10 min | P2 |
| 3 | Markdown ↔ ADF: existing library or in-house converter; which ADF nodes are supported losslessly? | In-house, covering paragraphs, headings, lists, code, links, emphasis, mentions; everything else = lossy | P2 |
| 4 | Is the `claude_code` provider compatible with Anthropic's terms for a distributed open-source tool? | Ship it as user-opt-in, documented as invoking the user's own CLI | Before P5 |
| 5 | Should newly imported issues land in `next` or `inbox`? | `next` (avoid inbox flood on first sync) | After a month of P2 use |
| 6 | Allow editing sprint and custom fields? | Read-only | Post-P4 |
| 7 | Network transport for remote clients (phone, web): TLS + token? | Not supported | When a remote client is planned |
| 8 | Import-merge into a non-empty database | Restore-to-empty only | Post-P5 |
| 9 | Stale thresholds (6.5) | As specified | After a month of P4 use |
| 10 | Drop the Node runtime dependency (single-executable TUI) | Node required | P5 |
| 11 | Jira Server / Data Center (PAT auth, API v2, wiki markup) | Unsupported | On demand |
| 12 | Multi-device sync beyond Jira | Out of scope; export/import only | — |
