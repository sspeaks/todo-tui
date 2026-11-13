# todo-tui

A keyboard-driven, local-first todo manager for the terminal. A Rust daemon owns all
state in SQLite and serves a gRPC API (`todo.v1`) over a Unix socket; an Ink TUI, the
`todo` CLI and an MCP server are clients. See [docs/design.md](docs/design.md).

Status: P1 is done (its People tools moved to P2, with Jira sync). Milestone M1 (capture) is
done: `todo add` with inline tokens, batch capture from stdin, and an offline spool.
Milestone M2 (undo) is done: `todo undo` and `todo redo`, relations between items in
the API, and deleting an item together with its subtasks. Milestone M3 (views) is
done: filters, sorting and paging, the six default views and their counts in the
API, and `todo status`. Milestone M4 (the TUI) is done: the views as tabs, a detail
pane, one-key triage, quick-add with a live preview, undo, bulk actions on marked
items, search and `$EDITOR` for descriptions; see [Using the TUI](#using-the-tui).
Milestone M5 (recurring items) is done: repeat rules with `@`, in captures and as
a TUI key, the next instance created when you complete one, and done items archived
after a week; see [Recurring items](#recurring-items). Milestone M7 (the MCP
server) is done, ahead of M6: `todo mcp` gives Claude Code tools to list, capture,
edit and complete items; see [Claude Code](#claude-code). Milestone M6 is done: a
config file with key remaps, views you make and edit with `:view` and query text,
daily backups and `todo restore`, and `todo export` / `todo import`; see
[Views](#views-of-your-own), [Configuration](#configuration) and
[Backups and export](#backups-and-export). Milestone M8 is done: an energy level
on items (`%low`, `e`), `h` / `l` between views, the L48 / N48 scrum report
(`todo l48`, `:l48`), and Teams meeting links (`M`); see
[Your scrum report: L48 / N48](#your-scrum-report-l48--n48) and
[Meetings](#meetings). The first P2 milestone is done: read-only sync from
Jira Data Center (`todo sync check|now|status`, `⇅` in the status bar, `o`).
The second (M10) pushes edits back: the title, priority, labels, due date
and status, with what Jira refuses or overwrites kept in a Suggestions pane
(`S`); M11 the description too, and M13 makes a todo a Jira issue (`P`) or
links it to one (`L`); M14 makes Jira's users People, so `~dana` and `w`
name a coworker and a meeting invites them; see [Jira sync](#jira-sync). The first Claude milestone (M12) refines
an item on request through your own Claude Code: `space r` or `todo refine`;
see [Claude refines an item](#claude-refines-an-item). A phone app (Expo) can
capture, browse the views and complete items over Tailscale; see
[Phone app](#phone-app). The CLI captures, undoes and counts;
listing, editing and completing items happen in the TUI or through Claude Code.

## Quick start

Linux (WSL2 included) or macOS, with:

- rustup. The first `cargo` run in the repo installs the Rust 1.97 toolchain pinned in
  `rust-toolchain.toml`. rustup also puts `~/.cargo/bin` on your `PATH`.
- Node 24, which includes corepack.
- `protoc` 3.15 or newer on `PATH`: `brew install protobuf`, or
  `sudo apt install protobuf-compiler` on Debian 12, Ubuntu 24.04 or newer. Older
  distros ship a protoc that is too old (Ubuntu 22.04 has 3.12). There, use a
  release from <https://github.com/protocolbuffers/protobuf/releases>.

```sh
git clone https://github.com/sspeaks/todo-tui && cd todo-tui   # or cd into your checkout
corepack enable                                # once per machine; provides the pinned pnpm
pnpm install                                   # first pnpm run asks to download pnpm 12.14.0: answer Y
pnpm gen                                       # generates the TS client code the TUI imports
pnpm -C tui build                              # bundles the TUI into tui/dist/tui.js
cargo install --locked --path crates/todo-cli  # builds `todo` into ~/.cargo/bin (a minute or so)
```

If `corepack enable` fails with `EACCES: permission denied, symlink …`, Node is
installed somewhere you cannot write, such as by Nix or a system package. Run
`corepack enable --install-directory ~/.local/bin` instead; any directory on your
`PATH` works. If `pnpm gen` fails with
`Cannot find module …/protoc-gen-es/bin/protoc-gen-es`, run `pnpm install --force`
and then `pnpm gen` again. See also [Troubleshooting](#troubleshooting).

Capture a todo, then open the TUI with `todo` and no arguments:

```console
$ todo add "call dentist ^fri +2 #home"
MTTVN5Y1  call dentist  +2 ^2026-10-30 #home
$ todo
1 Inbox 1 │ 2 Today │ 3 Next │ 4 Waiting │ 5 Someday │ 6 Archive
› ○ +2  call dentist                                                       ^Fri

NORMAL · ● live · u: capture "call dentist"          ? help · : command · q quit
```

`q` quits the TUI, and `?` lists its keys. `todo` runs the bundle that `pnpm -C tui build` made in this
checkout, so keep the checkout where it is. The first `todo add` (or the TUI)
starts the daemon in the background, and it keeps running after they exit.
`todo daemon stop` stops it.

## Install with Nix

With Nix (flakes enabled) you can skip the toolchain above. The flake builds `todo`
and the TUI bundle, and gives `todo` its own Node 24 for the TUI.

```sh
nix profile add 'git+file:///absolute/path/to/todo-tui?ref=main'   # your checkout
nix profile add github:sspeaks/todo-tui                            # once it is pushed
```

`?ref=main` builds the last commit on `main`, so half-finished work in the working
tree is never installed. Without it (a bare `git+file://…`, or `.`), Nix also picks
up uncommitted edits to tracked files and new files once they are `git add`ed.
Untracked files are never built.

To try it without installing, use `nix run` with the same URL and put arguments
after `--`, for example
`nix run 'git+file:///absolute/path/to/todo-tui?ref=main' -- add "call dentist ^fri"`.
In the checkout, `nix run .` runs the working tree and `nix build` leaves it in
`./result`.

After new commits on `main` (or a push), upgrade, then stop the daemon: it keeps
running the old build until it stops, and the next `todo add` or TUI starts the new
one.

```sh
nix profile upgrade todo-tui   # the name `nix profile list` shows (the checkout's directory name)
todo daemon stop
```

If you installed `todo` with `cargo install` before (Quick start), that copy in
`~/.cargo/bin` usually comes earlier on `PATH` than the Nix profile, so it keeps
running. To switch to the Nix one, stop the old daemon and remove the cargo copy:

```sh
todo daemon stop
cargo uninstall todo-cli
hash -r; command -v todo   # now ~/.nix-profile/bin/todo
```

## Using it

### Capture

```sh
todo add "call dentist ^fri +2 #home"            # `todo a` is an alias
todo add 'ask about the invoice ~"the vendor" >next ^+3d'
```

Each capture prints one line: the last 8 characters of the new item's id, the
title, then the fields it parsed.

```
TBSBZNDG  ask about the invoice  ^2026-10-26 ~"the vendor" >next
```

A capture can be up to 64 KiB (65536 bytes of UTF-8). Longer text is neither
sent nor spooled (below): `todo` prints
`todo: error: NOT CAPTURED: "<its first 60 characters>…": longer than 64 KiB`.

Plain words need no quotes (`todo a buy milk`), but quote anything with a token,
preferably in single quotes. Unquoted, the shell gets to some tokens first:

- `#home` can start a comment, so it is dropped.
- `>next` redirects the output to a file named `next`.
- zsh stops on `~dana` with `no such user or named directory: dana`.
- zsh with `extendedglob` expands `^fri` as a pattern.

Double quotes protect these tokens too. But they still expand `$`, so
`"pay $120 rent"` loses the `$120`. They also cannot hold `~"…"` unless you escape
the inner quotes.

| Token | Meaning | Examples |
|---|---|---|
| `+1` … `+5` | Priority, 1 = highest | `+2` |
| `^<date>` | Due date (forms below) | `^fri`, `^tom`, `^+3d`, `^eow`, `^nov2`, `^2026-11-02` |
| `#<label>` | Label; repeat for more | `#home #errand` |
| `~<person>` | Waiting on someone | `~dana`, `~"the vendor"` |
| `>next`, `>someday` | Status instead of the default `inbox` | `>someday` |
| `%low`, `%med`, `%high` | Energy: how much focus it takes, so `energy:low` finds what you can still do tired | `%low` |
| `@<rule>` | Repeats ([Recurring items](#recurring-items)) | `@daily`, `@mon,thu`, `@15th`, `@2w` |
| `\` | Escapes a sigil so it stays in the title | `fix \#1 fan` |

Everything else is the title. A capture that is only tokens (`todo add '+2 #home'`)
keeps the whole text as its title, and prints warnings saying so. Dates are
case-insensitive and resolve in local time:

| Date | Meaning |
|---|---|
| `today`, `tod`, `tomorrow`, `tom` | As named |
| `mon` … `sun` (3+ letters) | The next one strictly after today (`^fri` on a Friday is next week) |
| `+3d`, `+2w`, `+1m` | Days, weeks or calendar months from today |
| `eow`, `eom` | This week's Friday (next Friday on a weekend), last day of this month |
| `2026-11-02` | That date |
| `nov2`, `nov02` | The next Nov 2 (this year if not yet passed) |

A token that does not parse stays in the title, and `todo` prints a warning. The item
is still captured:

```console
$ todo add "renew passport ^nov2 ~dana ^bogus"
8CK2W4Z3  renew passport ^bogus  ^2026-11-02 ~dana
todo: warning: ^bogus: not a date
```

`todo add` exits 0 when every capture was stored or spooled (below), even with
warnings. It exits 1 in two cases:

- The text is blank: `todo: nothing to capture`.
- A capture was neither stored nor spooled, such as one over 64 KiB:
  `todo: error: NOT CAPTURED: …`. With several, the others are still captured.

### Several at once

With no text, `todo add` reads stdin and captures each non-empty line:

```console
$ printf 'water plants ^tom\nemail Sam #work +3\n' | todo add
DCXR7C58  water plants  ^2026-10-24
EYF9NP7Y  email Sam  +3 #work
$ todo add < meeting-notes.txt
```

At a terminal, `todo add` with no text prints
`todo add: each line is captured when you press Enter; end with Ctrl-D`. Each line
is captured as you enter it, and its result printed, so Ctrl-C loses only lines
whose result is not printed yet: normally just the one you have not finished, but
also the rest of a long paste that is still being captured. From a pipe or a file,
`todo add` reads all of stdin first, then captures the lines.

### When the daemon is down: the spool

If `todo add` cannot reach or start the daemon, it appends the capture to
`spool.jsonl` in the data directory, prints `spooled  <text>`, says why on stderr,
and exits 0:

```
spooled  pay rent ^tom #later
todo: daemon unavailable (…); spooled to …/data/spool.jsonl; it will be imported once the daemon is running again
```

The daemon imports the spool when it starts, before it answers any request, and
every 30 s while it runs. Relative dates resolve against the time you captured, not
the time of import. Lines it cannot import are moved to `spool.rejected.jsonl` next
to it.

### Undo and redo

```console
$ todo add "call dentist +2"
7GQ4ZK2M  call dentist  +2
$ todo undo
Undid: capture "call dentist"
$ todo redo
Redid: capture "call dentist"
```

`todo undo` reverts your most recent change, and `todo redo` re-applies the change
you undid last. Every capture and every change to an item is one step, including a
spooled capture once the daemon imports it. Undoing a capture deletes the item: it
disappears from the TUI, and `todo redo` brings it back. You can keep undoing, one
step at a time, back to the first change; a new change clears what there was to
redo. When there is nothing left, `todo` prints `Nothing to undo` (or
`Nothing to redo`). Changes made before you upgraded to a `todo` with undo are not
in the history, so undo stops at the first change made after the upgrade.

There is one history per data directory, shared by the CLI and the TUI: `todo undo`
also undoes a change made in the TUI, and the TUI's `u` also undoes a `todo add`. Deleting an item also deletes its subtasks, as one change, so one undo
restores them all.

Changes the daemon makes on its own are not in the history. If one of them has
changed a field since, undo leaves that field alone, restores the rest, and prints
a line for each field it left, with the item's short id:

```console
$ todo undo
Undid: edit "call dentist"
  not restored: due_date of 7GQ4ZK2M (the field has changed since)
```

If it could restore nothing at all, it says so, and that change also leaves the
history, so there is nothing to redo:

```console
$ todo undo
Could not undo: mark done "call dentist" (everything it changed has changed since)
  not restored: status of 7GQ4ZK2M (the field has changed since)
```

`todo undo` and `todo redo` start the daemon if needed, as `todo add` does, and
exit 0 when the daemon answers, with or without such lines. They have no spool: if
the daemon cannot be reached or started, they print `todo: daemon unavailable (…)`
and exit 1. If the daemon does not answer in time, they ask again, up to three times
in all; the daemon never applies the same request twice. If it still does not
answer, they print `todo: no answer from the daemon (…); the undo may have been
applied, so check before undoing again` and exit 1. They also exit 1, with
`todo: <why>`, if the daemon refuses.

### Counts: `todo status`

```console
$ todo status
📥 Inbox: 3
⏰ Due: 2 (1 overdue)
Views:
  Inbox: 3
  Today: 2
  Next: 0
  Waiting: 0
```

📥 counts items in the inbox and ⏰ items due today or overdue. Neither counts done,
cancelled, archived or deferred items. The views are the TUI's tabs that show a
count. `todo status` starts the daemon if needed, as `todo add` does.

`todo status --short` prints one line for a shell prompt or tmux status line, such
as `📥3 ⏰2`, leaving out zero counts. It never starts the daemon and gives up after
40 ms: with no daemon, a slow one or any error it prints nothing, and it always
exits 0. On a machine that uses a daemon elsewhere (below) it prints nothing:
reaching that daemon takes longer than a prompt can wait. In bash (in zsh, `setopt prompt_subst` first):

```sh
PS1='$(todo status --short) '"$PS1"
```

### Using the TUI

`todo` with no arguments opens the TUI, the way to see, edit and complete items. It
runs a single-file bundle of the TUI with `node`, which must be on `PATH`. Build the
bundle once, and again after the TUI changes:

```sh
pnpm -C tui build                      # writes tui/dist/tui.js
todo
```

The top line holds the views as tabs, with their counts; below it is the list of
the current view, with the selected item's details beside it in a terminal 110
columns or wider (narrower, `enter` opens them full screen and `esc` closes them).
Under the details, when the terminal is at least 22 rows tall, a calendar shows this
month and the next: today highlighted, and the days that have open items due in bold
(red when overdue).
The last two lines are a status bar (mode, connection, marks, what `u` would undo,
key hints) and a message line for prompts and results.

| Keys | What they do |
|---|---|
| **Navigation** | |
| `j` `k`, `↓` `↑` | Next / previous row (in the focused detail: scroll) |
| `gg` `G`, `home` `end` | First / last row |
| `ctrl-d` `ctrl-u`, `pgdn` `pgup` | Half a page down / up |
| `1`–`9`, `[` / `h`, `]` / `l` | Go to view N; previous / next view |
| `enter`, `tab` | Open the detail; switch focus between the list and the detail |
| `esc` | Back one step: close the detail, end `V`, clear `/`, clear the marks |
| **Items** | |
| `a`, `A` | Quick-add; quick-add a subtask of the selected item |
| `i` | Edit the title |
| `E` | Edit the description in `$EDITOR` |
| `p` then `1`–`5`, `p0` | Set the priority; clear it |
| `e` then `l` `m` `h`, `e0` | Set the energy (low, medium, high: `▁ ▄ █` after the priority); clear it |
| `^`, `D` | Set the due date; defer until a date |
| `w`, `W` | Set who you are waiting on (and when to follow up); clear it |
| `s` | Set the status: `i` inbox, `n` next, `p` in progress, `s` someday, `d` done, `c` cancelled |
| `x`, `X` | Mark done; mark cancelled |
| `t` | Edit the labels |
| `@` | Set the repeat rule; empty stops it |
| `z` | Archive / unarchive |
| `dd` | Delete, with its subtasks (asks first: `y` deletes) |
| `M` | Schedule a Teams meeting about it ([Meetings](#meetings)) |
| `o` | Open its Jira issue in the browser ([Jira sync](#jira-sync)) |
| `P`, `L` | Make it a Jira issue; link it to one by key ([Jira sync](#jira-sync)) |
| `S`, `:suggestions` | The Suggestions pane: what Jira or Claude left to review ([Jira sync](#jira-sync)) |
| `space r`, `:refine` | Ask Claude to refine it, or the marked ones ([Claude refines an item](#claude-refines-an-item)) |
| **Marks and bulk** | |
| `m` | Mark or unmark the item, and move down |
| `V` | Mark the rows you move over; `V` or `esc` ends it |
| `x X s p e ^ D w W t z dd @` | With marks: act on every marked item at once, as one undo step (`t` then takes `+a -b` to add and remove labels); `M` makes one meeting about all of them |
| **Search** | |
| `/` | Narrow this view as you type; `enter` keeps it, `esc` clears it |
| `:search <words>` | Search everything, done, archived and deferred items too, in a `Search:` tab |
| **Undo** | |
| `u`, `ctrl-r` | Undo, redo: the same history as `todo undo` |
| **Commands** | |
| `:` | Command line: `:q`, `:help`, `:search <words>`, `:refresh`, `:view …` ([Views](#views-of-your-own)), `:config`, `:export [path]`, `:l48 [since]`, `:meet`, `:sync [status\|outbox]`, `:suggestions`, `:refine`, `:promote [PROJECT]`, `:link KEY`; `tab` completes |
| `gs` | Sync with Jira now |
| `?`, F1, `:help` | Help: every key, the capture syntax and the dates; `/` in it searches as you type (`enter` keeps the search, `esc` clears it) |
| `ctrl-z` | Suspend to the shell; `fg` comes back |
| `q`, `ctrl-c` | Quit |

**Quick-add** (`a`) takes the same tokens as `todo add` ([Capture](#capture)), and
the line under it shows what `enter` will capture, with a token it cannot apply in
red and the reason. `enter` adds and leaves the line open for the next one; `enter`
on an empty line or `esc` closes it. Pasting several lines adds one item per line.
Changes made elsewhere, by `todo add` or another TUI, show up live.

**Prompts.** `^` and `D` start from the current date (`2026-10-30`) and take the
same dates as `^` in a capture, with a preview; an empty date clears it. `w` takes
`person ^date`, the date being when to follow up and optional; `tab` completes a
name you have used before, and an empty line stops waiting. `t` edits the labels as
`#a #b`. `i` edits the title as plain text, so its tokens are not parsed. In every
prompt `enter` saves and `esc` cancels.

**`E`** opens the description in `$VISUAL`, else `$EDITOR`, else `vi`; save and quit
to store it. Quitting without saving, or an editor that exits with an error (`:cq`
in vim), changes nothing. A graphical editor needs its wait flag:
`EDITOR="code --wait"`. If someone changed the description while you were editing,
your text is not lost: it is saved in `recovered/` next to `daemon.log` (see
[Where data lives](#where-data-lives)), and the message line names the file.

**Quitting**: `q`, `:q` or `ctrl-c`. In quick-add or a prompt, `ctrl-c` cancels it
instead. The TUI uses the terminal's alternate screen, so your shell is as you left
it.

`todo` uses the first bundle that exists of:

1. `$TODO_TUI_ENTRY`, a path to a `tui.js`. If it is set (and not empty), that
   file must exist; otherwise `todo` stops with
   `todo: TODO_TUI_ENTRY is <path>, which is not a file`.
2. `../share/todo-tui/tui.js` from the directory holding `todo`, for future
   packages.
3. `tui/dist/tui.js` in the checkout `todo` was built from. If that is missing
   too, `todo` stops with
   ``todo: the TUI is not built; run `pnpm -C <checkout>/tui build` (or set TODO_TUI_ENTRY)``.

To run the TUI from source instead, without building, from the repo:

```sh
pnpm -C tui dev
```

If no daemon is running, the TUI starts one with `todo daemon --detach`, using
`$TODO_BIN` if it is set and `todo` on `PATH` otherwise. If the daemon goes away,
the status bar says `◌ reconnecting`, the list dims and the TUI starts it again;
a change you make meanwhile is sent once it is back, or the message line says
`Not saved: daemon unavailable` and your typed text stays. `todo` sets `TODO_BIN` to
itself unless it is already set, so the TUI starts that `todo`'s daemon. Under
`pnpm -C tui dev` with neither, it stops with
`todo-tui: cannot run todo: spawn todo ENOENT`. Give `TODO_BIN` as an absolute path:
`pnpm -C tui dev` runs from `tui/`, so a relative one does not resolve.

### Recurring items

An `@` token in a capture makes the item repeat:

| Rule | Repeats |
|---|---|
| `@daily` | Every day |
| `@weekdays` | Monday to Friday |
| `@mon,thu` | On those weekdays (each 3+ letters) |
| `@weekly`, `@monthly` | On the weekday, or the day of the month, of its `^` date (else today) |
| `@15th` | That day of every month, or the month's last day when it is shorter (`@31st`) |
| `@3d`, `@2w` | That long after you complete the previous one |

Without a `^` date, the first date on the schedule becomes its due date (an `@3d` or
`@2w` item has none; the next one is due that long after you complete it). `todo add`
prints the rule as a token, a `weekly` or `monthly` with its day:

```console
$ todo add 'water plants @mon,thu'
KFXWF8X6  water plants  ^2026-11-02 @mon,thu
$ todo add 'review budget ^fri @weekly'
YKZ4GCVW  review budget  ^2026-11-06 @fri
```

A rule that does not parse stays in the title, with
`todo: warning: @home: not a repeat rule`; an `@` inside a word (`bob@x.com`) is
title text, and `\@` escapes one.

In the TUI, `@` sets the rule of the selected item (or the marked ones), prefilled
with its rule (`mon,thu`) and previewed (`→ every Mon, Thu · first Mon Nov 2`).
`enter` on an empty line stops the repeat. The details show it as `Repeats`, and the
row has a `🔁`.

`x` (or `X`, which skips one) creates the next instance, with the same title,
priority, labels and rule, and the message line says when:
`mark done "water plants" · next Thu Nov 5 · u undo`. If you were late, the dates you
missed are skipped: the next one is never before today. Unless that is today, it is
deferred until its day, so it stays out of the views until then (`:search` finds it). One `u` undoes
both, reopening the item and removing the next one. To end a series, stop its repeat
with `@`, or delete the open instance with `dd` (`u` brings it back, and the series
with it). Changing the rule changes the open instance and keeps its due date; the ones you
completed keep theirs. `@` on a done or cancelled item is refused: reopen it first.

Done and cancelled items, recurring or not, show in the Archive tab straight away,
and the daemon archives them 7 days after they closed. A `z` after closing one
sticks: unarchived, it is not archived again until you close it again. Reopening an
item it archived takes it out of the archive. The 7 days are
`[ui] archive_done_after_days` in the [config](#configuration).

### Views of your own

The tabs are saved views, and `:view` makes, edits, renames, moves and deletes them.
A view's filter is query text: words that are ANDed, previewed as you type with the
number of items they match (or the first error, coloured where it is).

```text
:view new Urgent is:next,in_progress priority>=high sort:-priority
:view new Deferred has:defer_until include:deferred sort:defer_until
:view new Work #work any:(is:next has:waiting) count:off
:view edit Urgent             # opens with the view's query; enter saves
:view rename Urgent Hot       # :view rename <new> renames the current tab
:view move Hot first          # or a tab number, left, right, last
:view delete Hot              # asks first
```

| Term | Means |
|---|---|
| `is:next,in_progress` | status is one of these |
| `#work` | has the label |
| `priority>=high`, `due_date<=today+3d` | compare a field: `:` (one of), `:=`, `!=`, `<`, `<=`, `>`, `>=` |
| `has:waiting`, `no:due_date` | the field is set, or unset |
| `milk`, `"two words"`, `description~notes` | title (or another text field) contains the words |
| `-is:done`, `-has:waiting` | not (only for `:` and `has:`/`no:`) |
| `any:( … )`, `( … )` | one of the terms inside, or all of them |
| `include:done,archived,deferred` | lift the rules that hide those items |
| `sort:-priority,due_date` | sort, `-` for descending |
| `count:off` | no count on the tab |

Field names are the item's fields (`due_date`, `waiting.on`, `effective_priority`,
`focus_week`, …); dates take `today`, `week_start` and `week_end`, each with
`+3d` or `-2w`. `:view edit Today` shows how a default view is written. The full
grammar is design 4.8, "Query text".

### Configuration

`config.toml` is optional (`todo config path` prints where it goes). Only the daemon
reads it: at start, on `todo config reload`, and on SIGHUP. A bad value is a warning
that names its key, and the default is used; `todo config check` prints the
warnings without the daemon, and `:config` in the TUI shows them.

```toml
[ui]
archive_done_after_days = 7   # done items move to the Archive after this many days

[keymap]                      # a key sequence and the action it runs
"K" = "move_up"               # action ids: the help (?) lists the keys; design 2.5 the ids
"g d" = "mark_done"           # keys separated by spaces; ctrl+x, enter, esc, tab, …
"x" = ""                      # an empty action unbinds the key

[backup]
enabled = true
retain_days = 14

[l48]                         # the scrum report's window
window_days = 2               # working days back (L48) and ahead (N48)
working_days = ["mon", "tue", "wed", "thu", "fri"]

[meetings]
provider = "teams"            # "none" turns M and :meet off
```

A remap that would make one key both a whole binding and the start of another (`p`
is the start of `p 1`) is skipped with a warning. The help and the hints show the
keys in effect.

### Your scrum report: L48 / N48

`todo l48` prints what you worked on in the last two working days and what is
planned for the next two, ready to paste:

```
L48:
- Release 2.3 prep: 3 of 5 subtasks done
- Fix the flaky login test (PLAT-412)
N48:
- Write the planning memo
- Close out the database migration; waiting on Sam
```

- **L48** is the items you completed, started or edited in the window, newest
  first, one line per top-level item, with its subtasks done in the window counted.
  Capturing an item is not work, and nor is triage (labels, priority, energy, the
  focus week); undone changes do not count.
- **N48** is the open items in progress, due by the end of the next two working
  days, waiting with a follow-up by then, or in this week's focus, ordered as Today.
- The window counts working days, so Monday's L48 covers Thursday and Friday.
  `--since thu` (or `-3d`, `oct29`, `yesterday`, a date) starts it elsewhere;
  `[l48]` changes the window.
- `--copy` also puts it on the clipboard (`pbcopy`, else `wl-copy` or `xclip`).
  In the TUI, `:l48` shows it and `y` copies it; Claude Code has `todo_l48`.

### Meetings

`M` (or `:meet`) opens Microsoft Teams' new-meeting form, filled in from the item:
the title as the subject (edit it in the prompt first), the description's first
paragraph and Jira link as the content, and who it waits on as the attendee when
that is an email address (`~dana@example.com`). With marks it is one meeting about
all of them. On macOS it opens the Teams app (`msteams:`), else the browser. You
pick the time and send it in Teams; todo-tui creates nothing and needs no login. `[meetings] provider = "none"` turns it off.

### Jira sync

Your assigned Jira Data Center issues can live next to your todos: they are
imported as items, with their key, status and link, and kept fresh (every
minute while the TUI is open, every ten otherwise). Some edits go back to
Jira: the title, priority, labels, due date and status. Everything else
about the issue stays Jira's, and what is yours stays here.

1. Make a Personal Access Token in Jira: your avatar → Profile → Personal
   Access Tokens → Create token. It acts as you, so what it pushes is your
   edit in Jira's history.
2. Keep it in the macOS keychain (it prompts for the token):

   ```sh
   security add-generic-password -s todo-tui -a work-jira -w
   ```

3. Add the instance to `~/.config/todo-tui/config.toml`:

   ```toml
   [[instances]]
   id = "work-jira"
   kind = "jira-dc"
   base_url = "https://jira.example.com"
   credential = "keychain:todo-tui/work-jira"   # service/account; or env:NAME, or file:PATH (mode 0600)
   # The default scope: your open issues.
   scopes = [{ name = "mine", query = "assignee = currentUser() AND resolution = Unresolved" }]
   # push = false                 # read-only: nothing here changes Jira
   # local_fields = ["due_date"]  # due dates stay here: for a Jira that refuses them
   # done_resolution = "Done"     # what x closes with; cancel_resolution = "Won't Do" for X
   # transitions = { todo = "Open", in_progress = "In Progress" }   # else the first Jira offers
   ```

   `todo config reload` (or a SIGHUP) picks it up. Pushes are on by default;
   set `push = false` first if you want to try it read-only.
4. `todo sync check` asks Jira who the token is, without the daemon:
   `work-jira: ok as Dana Example (JIRAUSER10100), time zone America/Los_Angeles`.
5. `todo sync now` pulls and waits for it; `todo sync status` says how it
   stands (`ok · 12 items · last pull 3m ago`, `offline, retrying`, or a
   refused token). Both exit non-zero when something needs you.

In the TUI, the status bar shows `⇅ work-jira 2m`, `o` opens the selected
issue in the browser, `gs` (or `:sync`) pulls now and `:sync status` says
how it stands. To Do and In Progress issues land in Next; a resolution of
Won't Do, Duplicate and the like (`cancelled_resolutions`) makes one
cancelled, any other done.

**What reaches Jira.** On a Jira item, `i` (title), `E` (description), `p`
(priority), `t` (labels), `^` (due date) and the status keys change the issue
too, within seconds while Jira answers:

- `x` closes it with resolution Done, `X` with Won't Do, `s p` starts it,
  and `s i` / `s n` / `s s` from in progress or done move it back to To Do.
  Between inbox, next and someday the move is only here.
- The description is Markdown here and wiki markup in Jira, converted both
  ways. Formatting Markdown has no form for (a colour, a panel, an odd
  table) is kept as it was, as a `` `wiki:…` `` code span or a
  ```` ```wiki ```` block: leave those be, and they go back unchanged.
- Undo pushes the old value back.
- An issue assigned to someone else asks first ("PLAT-4 is assigned to
  Alex: change it in Jira? y/N").
- Offline, or before Jira answers, the change waits: the row shows `↑`, the
  status bar `⇅ work-jira ↑2`, and it goes once Jira is back.

**Jira's alone**: the assignee, components, fix versions, parent, story
points and estimates. The TUI refuses them ("PLAT-1: the assignee is
Jira's: change it there (o opens it)"), and a bulk edit
leaves those items out. **Yours alone**, never sent: waiting, defer, notes,
the local priority override, energy, archive and delete. Deleting an item
here only hides it, and drops its changes still waiting; the issue stays
open in Jira. An issue that leaves your scopes is archived on its own, and
one deleted in Jira (or that you lost access to) is marked unavailable.

**When Jira wins.** If someone changes a field in Jira while your edit of it
waits, Jira's value stands and yours is kept as a Suggestion. If Jira
refuses a change (a workflow that cannot close from there, a field it will
not take), Jira's value stands too, the row shows `!` and the status bar
`!1`. `S` (or `:suggestions`) opens the Suggestions pane: `j`/`k`, `enter`
ticks a change (a heading: all of it), `ctrl-s` applies what is ticked,
which pushes it again, and `r` lets it go. The status bar shows `✦ 1` while
one waits. `:sync outbox` and `todo sync outbox` list what waits and what
was refused; `todo sync status` adds `2 to push, 1 refused` and exits
non-zero while something is refused.

**Making a todo a Jira issue, or linking it to one.** On a local todo, `P`
(or `:promote [PROJECT]`, or `todo promote <id>`) creates the issue in Jira
from its title, description, priority, labels and due date, and links the
todo to it; the prompt is prefilled with `promote_default`'s project:

```toml
[[instances]]
# …
promote_default = { project = "PLAT", issue_type = "Task" }
# subtask_issue_type = "Sub-task"   # a subtask of a linked todo is created under it
```

The issue is real and your team sees it; it cannot be undone from here. If
Jira needs more fields, it says which. If Jira does not answer, the daemon
looks for the issue before saying it failed, so a retry never makes two.
`L` (or `:link KEY`, or `todo link <id> KEY`) links a todo to an issue that
exists, yours or a teammate's: Jira's fields replace the todo's, and what
differed waits in `S` to put back. Either way the todo stays in your list,
even outside your scopes.

**People.** Each Jira user sync sees (assignees, reporters, and you) is a
person here, with their name and address. `~dana` in a capture waits on
the one person whose username, email, full name or first name is "dana";
when two answer to it, it stays text and the preview says so. In `w`, `tab`
completes from the people known and the names you have waited on; a name
no one here has makes `tab` ask Jira's directory, and `enter` adds the
person it found. Names typed for anyone else (`~"the vendor"`) stay text,
as before. A person's address goes into `M`'s meeting. `todo people
[name] [--jira]` lists them.

**Turning it off.** `push = false` on the instance makes it read-only again:
nothing more is sent, the title and the rest are refused as Jira's, and
what was waiting goes once pushes are back on. `local_fields = ["due_date"]`
keeps due dates here, for a Jira whose screens have no due date: setting
one never touches Jira and sync never overwrites it.

### Backups and export

The daemon backs up the database when it starts and once a day after, to
`backups/` in the data directory, and deletes backups older than `retain_days`.
It also backs up before it migrates the database to a new schema.

| Command | What it does |
|---|---|
| `todo backup now` | Backs up at once, also when automatic backups are off |
| `todo backup list` | Lists the backups |
| `todo restore FILE [--yes]` | Puts a backup in place: it asks first, saves the current database as `todo-prerestore-….db`, and restarts the daemon. Close other TUIs first. Restoring that `prerestore` file undoes it |
| `todo export [--out FILE] [--no-history]` | Every record as JSON Lines (`--no-history` leaves out the undo history); `:export [path]` in the TUI |
| `todo import FILE` | Loads an export into an empty database, as on a new machine |

### Claude refines an item

With Claude Code installed and logged in, Claude can tidy an item for you:
a clearer title, and the due date, priority, labels and who it waits on that
its text gives. It only proposes; you tick what you want.

1. Turn it on in `~/.config/todo-tui/config.toml`, then `todo config reload`:

   ```toml
   [assistant]
   provider = "claude_code"
   # model = "claude-haiku-5-5"     # else Claude Code's own default
   # style_notes = "titles imperative, under 60 characters"
   # include_private_notes = false  # private notes are never sent unless true
   ```

2. In the TUI, `space r` on an item (or the marked ones), or `:refine`. The
   status bar shows `✦ refining…`, and a toast says when it is done.
3. `S` opens the Suggestions pane: Claude's changes are already ticked;
   `enter` unticks one, `ctrl-s` applies the rest as one change (`u` undoes
   it), `r` rejects them all. An item that looks like a duplicate is named
   there.

From the shell, `todo refine <id>` (the short id `todo add` prints) waits
and prints what Claude proposes; `todo assistant usage` lists the requests
and what they cost. Nothing runs by itself: each refine is one you asked
for. The daemon runs your `claude` CLI headless with no tools, so Claude
sees only the item, the labels you use and the titles of similar items,
billed to the plan Claude Code is logged into
([ADR-0010](docs/adr/0010-claude-code-provider.md)).

### Claude Code

`todo mcp` serves the todo tools to Claude Code over stdio. Register it once:

```sh
claude mcp add --scope user todo -- todo mcp
```

| Tool | What it does |
|---|---|
| `todo_list` | Items in a view (`today`, `next`, `waiting`, …), narrowed by text or labels |
| `todo_get`, `todo_status`, `todo_views` | One item in full; the counts; the views |
| `todo_l48` | The L48 / N48 report, as `todo l48` prints it |
| `todo_meeting_link` | A Teams new-meeting link for items, prefilled as `M` fills it |
| `todo_suggestions` | What waits to review (Jira conflicts and refusals, Claude's refines); applying one is yours, in the TUI's `S` |
| `todo_promote`, `todo_link` | Make a todo a Jira issue (ask first: it is real); link one to an issue by key |
| `todo_people` | The people sync knows, with a `person_id` for `todo_update`'s `waiting_on: "person:<id>"`; `jira: true` also asks Jira's directory |
| `todo_capture` | A capture, as `todo add` takes it (tokens and all) |
| `todo_update` | Title, description, priority, local priority, energy, due, defer, labels, waiting, private notes; on a Jira item the title, description, priority, labels and due date reach Jira |
| `todo_set_status` | `inbox`, `next`, `someday`, `in_progress`, `done`, `cancelled`; completing a repeating item returns the next one |
| `todo_set_repeat` | Sets or clears the repeat rule |
| `todo_archive`, `todo_unarchive`, `todo_delete`, `todo_undelete` | As named |
| `todo_undo`, `todo_redo` | The same undo stack as `todo undo` and `u` in the TUI |

Tools take the full id or the 8-character short id `todo add` prints. Their captures
are `created_via = mcp`, and every change is yours, so `todo undo` or `u` undoes it.
`todo mcp --read-only` offers only the tools that change nothing: `todo_list`,
`todo_get`, `todo_status`, `todo_views`, `todo_l48`, `todo_meeting_link` and `todo_suggestions`. Like the TUI, it runs a bundle with Node (`share/todo-tui/mcp.js`, or
`mcp/dist/mcp.js` in a checkout; `TODO_MCP_ENTRY` overrides it) and starts the daemon
if needed.

[`claude/skills/todo/`](claude/skills/todo/SKILL.md) is a Claude Code skill that
teaches Claude the capture syntax, the views, recurrence and Jira-linked items: what
an edit pushes, and to ask before closing or renaming a ticket as a side effect. To use it everywhere: `ln -s ~/todo-tui/claude/skills/todo ~/.claude/skills/todo`.

### Phone app

`apps/mobile` is an Expo (React Native) app that talks to the daemon on your
computer over [Tailscale](https://tailscale.com) ([ADR-0008](docs/adr/0008-remote-transport.md)).
It shows the views as tabs with the same rows as the TUI, in its look: the TUI's cyan,
JetBrains Mono for dates, labels, counts and ids, a row's priority as its coloured left
edge, and light or dark with the phone. To add an item, tap the
round + button, type a title and set the rest with one tap each (due, priority,
status, repeat, the labels and names you use), or type the capture syntax; a preview
shows what will be captured. The item screen has the same one-tap choices.
Tap the ring (or swipe right) to complete an item and the check to reopen it; swipe left to delete.
The tab bar has the list, Search, and Settings (the connection and pairing).
What you add while your computer is out of reach is kept on the phone and sent
once it is back. Tap an item to edit its title, status,
priority, due date, repeat, labels, waiting on and follow-up, hide-until date and
notes, or to delete it. Every change can be undone from the snackbar. On a Jira
item the same edits reach Jira as they do from the TUI ([Jira sync](#jira-sync)),
and one assigned to someone else asks first. It is a thin client: it keeps only the list it
last showed, so a launch shows that at once (the header says `updating…` until your computer
answers), and so does a launch while your computer is asleep or off the tailnet.

Set it up once on the computer:

1. Turn the remote listener on: add this to `~/.config/todo-tui/config.toml`
   (`$XDG_CONFIG_HOME/todo-tui/` if set, on macOS too; `todo remote status` prints
   the path), then `todo daemon stop`. The next `todo` command starts the daemon
   again with the listener on.

   ```toml
   [remote]
   listen = "127.0.0.1:7743"
   ```

2. Give it HTTPS on your tailnet. The listener is loopback only; Tailscale adds the
   certificate and keeps it to your devices:

   ```sh
   tailscale serve --bg --https=443 http://127.0.0.1:7743
   ```

   With the macOS Tailscale app, the CLI is
   `/Applications/Tailscale.app/Contents/MacOS/Tailscale`. The flake's dev shell has a
   `tailscale` CLI too, but on macOS it only reaches an open-source `tailscaled`, not
   the app.

3. Pair the phone: `todo remote pair --name phone` prints a QR code, the server
   (`https://<computer>.<tailnet>.ts.net`, found with `tailscale status`; `--url`
   overrides it) and a token, which is shown only once.

Then run the app (the phone needs Tailscale on, and Expo Go from the App Store or
Play Store):

```sh
pnpm -C apps/mobile start     # scan the Expo QR code with the phone's camera
```

In the app, tap **Scan QR code** and scan the pairing code. `todo remote list` shows
the paired devices and `todo remote revoke phone` unpairs one at once; the app then
asks to pair again. `todo remote status` checks the setup. Every call needs the
token, and the phone cannot stop the daemon.

#### Install it on your iPhone (no dev server)

With Expo Go the app runs only while `pnpm -C apps/mobile start` does. To install
it as an app of its own:

- **Free, with Xcode** (the install lasts 7 days, then run it again): install
  CocoaPods once (`brew install cocoapods`), plug the iPhone in, turn on Developer
  Mode (Settings › Privacy & Security), then

  ```sh
  pnpm -C apps/mobile ios:device    # expo run:ios --device --configuration Release
  ```

  Pick the iPhone, and your Apple ID as the team when asked. The first time, trust
  the developer in Settings › General › VPN & Device Management. With an install of
  its own, `todo-tui://pair` links from `todo remote pair` open the app.

- **TestFlight** (Apple Developer Program): `npx eas-cli@latest build -p ios
  --profile production` from `apps/mobile`, then `npx eas-cli@latest submit -p ios`
  (`eas.json` has the profiles).

### A daemon on another machine

The daemon can live on an always-on machine on your tailnet, and every client
can use it: the phone, and each laptop's `todo`, TUI and Claude Code tools
([ADR-0008](docs/adr/0008-remote-transport.md)).

**Host it on NixOS** with the flake's module:

```nix
{
  inputs.todo-tui.url = "github:sspeaks/todo-tui";
  inputs.sops-nix.url = "github:Mic92/sops-nix";
  outputs = { nixpkgs, todo-tui, sops-nix, ... }: {
    nixosConfigurations.box = nixpkgs.lib.nixosSystem {
      modules = [
        todo-tui.nixosModules.default
        sops-nix.nixosModules.sops    # with your sops.defaultSopsFile and keys
        ({ config, ... }: {
          services.tailscale.enable = true;
          sops.secrets.jira-pat = { restartUnits = [ "todo-tui.service" ]; };
          services.todo-tui = {
            enable = true;
            tailscaleServe = true;    # https://box.<tailnet>.ts.net
            timeZone = "America/Los_Angeles";    # yours: see below
            settings.backup.retain_days = 30;
            # Jira, its token from sops-nix (or agenix): handed to the
            # daemon as a systemd credential, never put in the Nix store.
            credentials.jira = config.sops.secrets.jira-pat.path;
            settings.instances = [{
              id = "work-jira";
              kind = "jira-dc";
              base_url = "https://jira.example.com";
              credential = "systemd:jira";
            }];
          };
        })
      ];
    };
  };
}
```

The secret keeps sops-nix's defaults (root, 0400): the module passes it with
`LoadCredential=`, so it needs no owner change. `restartUnits` picks up a rotated
token.
`credential = "systemd:NAME"` works for any secret `credentials` names;
`environmentFile` (with `env:NAME`) is there for the rest.

It runs as its own `todo-tui` user with `/var/lib/todo-tui` as its home, the
listener on `127.0.0.1:7743` and `tailscale serve` in front of it (tailnet HTTPS
certificates must be on; the serve unit waits for tailscale to log in, and keeps
retrying). `systemctl reload todo-tui` rereads the config. On the box,
`sudo todo-tui-admin …` runs `todo` as the service, so pair each device with
`sudo todo-tui-admin remote pair --name laptop` (the phone scans the QR code). It
never starts a daemon of its own: `todo-tui-admin daemon stop` and `restore` go
through `systemctl`, and with the service stopped the rest say so.

The daemon works out relative dates (`^tom`, `^fri`) and "today" (Today, overdue,
L48) in its own time zone, for every device: set `timeZone` (or the box's
`time.timeZone`; NixOS defaults to UTC) to yours, or an evening capture lands a
day late.

**Use it from a laptop:** keep the token in the keychain and point the config at
the box:

```sh
security add-generic-password -s todo-tui -a remote -w      # paste the token
```

```toml
# ~/.config/todo-tui/config.toml
[client]
remote = "https://box.<tailnet>.ts.net"
credential = "keychain:todo-tui/remote"
```

Then `todo`, `todo add`, `todo status`, `todo mcp` and the rest use the box; no
daemon starts on the laptop. `TODO_REMOTE` and `TODO_REMOTE_TOKEN` do the same for
one shell. What acts on a machine's own daemon (`todo daemon`, `restore`,
`backup list`, `remote`) refuses there: run it on the box. `todo daemon stop`
still stops one left running on the laptop. A `[client]` table that is broken (no
credential, a typo, a bad address) is refused too, rather than quietly using a
daemon on the laptop. A capture made while the box was out of reach is spooled on
the laptop (in `spool.remote.jsonl`) and sent with the next `todo add` that
reaches it; the TUI and Claude Code tools do not send it.

### The daemon

You rarely need to manage it: `todo add`, `todo undo`, `todo redo`, `todo status`,
`todo mcp` and the TUI start it when needed, and only one daemon runs per data directory.

| Command | What it does |
|---|---|
| `todo daemon --detach` | Starts it in the background; prints `todo daemon: started` or `todo daemon: already running` |
| `todo daemon` | Runs it in the foreground until Ctrl-C. It prints nothing; it logs to `daemon.log` |
| `todo daemon stop` | Stops it and waits for it to exit; prints `todo daemon: stopped` or `todo daemon: not running` |

There is no `status` command yet. The log is `daemon.log` in the logs directory
(below); set `TODO_TUI_LOG=debug` (a `tracing` filter, default `info`) in the
environment the daemon starts from for more detail.

After installing a new build of `todo`, run `todo daemon stop`; the next `todo add`
or TUI starts the new daemon. A running daemon keeps its old code until then. (The
CLI and TUI restart it on their own only when it is too old for their API version.)

### Where data lives

| What | Linux | macOS |
|---|---|---|
| Database `todo.db`, `daemon.lock`, `spool.jsonl`, `backups/` | `$XDG_DATA_HOME/todo-tui/`, default `~/.local/share/todo-tui/` | `~/Library/Application Support/todo-tui/` |
| `config.toml` | `$XDG_CONFIG_HOME/todo-tui/`, default `~/.config/todo-tui/` | the same |
| `daemon.log` | `$XDG_STATE_HOME/todo-tui/`, default `~/.local/state/todo-tui/` | `~/Library/Logs/todo-tui/` |
| Socket `daemon.sock` | `$XDG_RUNTIME_DIR/todo-tui/`, or `/tmp/todo-tui-$UID/` | `$TMPDIR/todo-tui/` |

### Trying it on a scratch home

`TODO_TUI_HOME=<dir>` puts everything under one directory: `<dir>/data/`,
`<dir>/logs/`, and the socket in `<dir>/run/` (or in `/tmp/todo-tui-$UID/h-<hash>/`
when that path would be over 100 bytes). Each home gets its own daemon, so this
leaves your real todos alone. Use an absolute path: the CLI and the TUI resolve a
relative one against their own working directories, which differ under
`pnpm -C tui`.

```sh
export TODO_TUI_HOME=$(mktemp -d)
todo add "try me +1"
todo
todo daemon stop
rm -rf "$TODO_TUI_HOME"; unset TODO_TUI_HOME
```

## Layout

- `proto/` — the `todo.v1` contract (Buf)
- `crates/` — Rust workspace: core, store, sync, jira, assistant, proto, daemon, cli
- `packages/client/` — shared TypeScript client (Connect-ES); `./portable` is the
  Node-free part, with the gRPC-Web client for the remote listener
- `packages/present/` — how items read, shared by the TUI and the phone app
- `tui/`, `mcp/` — the Ink TUI and the MCP server
- `apps/mobile/` — the phone app (Expo)
- `claude/skills/todo/` — the Claude Code skill for managing todos through the MCP server (not in `skills/`, which symposium would sync into `.claude/skills/`)
- `scripts/smoke.sh` — end-to-end smoke test

## Development

Requires the same as [Quick start](#quick-start): rustup, Node 24 with corepack
(`corepack enable`), and `protoc` 3.15 or newer on `PATH`.

With Nix, `nix develop` replaces that setup: it opens a shell with cargo, rustc,
clippy, rustfmt, rust-analyzer, protoc, Node 24, pnpm and buf on `PATH`. Its Rust is
nixpkgs' and can be newer than the 1.97 in `rust-toolchain.toml` that CI uses, so
clippy may report lints CI does not. pnpm still runs the version pinned in
`package.json`. When `pnpm-lock.yaml` changes, update the hash in `flake.nix` (the
comment at its top says how).

```sh
pnpm install
pnpm gen              # TypeScript codegen into packages/client/src/gen (gitignored)
pnpm lint:proto       # buf lint, and buf breaking against the main branch
pnpm typecheck
pnpm test
cargo build --workspace
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
pnpm smoke            # builds todo, then exercises daemon, CLI and TS client together
```

Run `pnpm gen` again after changing `proto/`; the Rust side compiles the protos
during `cargo build` (that is what needs `protoc`).

`pnpm test` skips the client tests that run against a real daemon
(`packages/client/test/uds.integration.test.ts`) unless `TODO_BIN` is an absolute
path to a built `todo`:
`cargo build -p todo-cli && TODO_BIN=$PWD/target/debug/todo pnpm test`. The snippet
below and `pnpm smoke` assume the default `target/`. If you set
`CARGO_TARGET_DIR`, smoke fails with `…/target/debug/todo: No such file or directory`,
so run `TODO_BIN=$CARGO_TARGET_DIR/debug/todo pnpm smoke` instead.

To use your working copy instead of the installed `todo`, build it and put it first
on `PATH`, then stop the old daemon so the new one starts:

```sh
cargo build -p todo-cli
export PATH="$PWD/target/debug:$PATH"
todo daemon stop
```

## Troubleshooting

- Building `todo-proto` fails with ``Could not find `protoc` ``. Install protoc (see
  [Quick start](#quick-start)), or set `PROTOC` to its path.
- Building `todo-proto` fails with
  `This file contains proto3 optional fields, but --experimental_allow_proto3_optional was not set`.
  Your protoc is older than 3.15.
- `pnpm gen` fails with `Cannot find module …/protoc-gen-es/bin/protoc-gen-es`. The
  installed packages are incomplete, and a plain `pnpm install` only reports
  "Already up to date". Run `pnpm install --force`, then `pnpm gen`.
- The TUI fails with `Cannot find module …/packages/client/src/gen/…`. Run
  `pnpm gen`.
- `todo` stops with `todo: the TUI is not built; …`. Run the `pnpm … build` it
  names, or `pnpm -C tui dev` from the repo.
- `todo` stops with `todo: cannot run the TUI: node is not on PATH; …`. Install
  Node 24 or newer.
- `todo` opens an old TUI after a `git pull`. Run `pnpm -C tui build` again.
- The TUI stops with `todo-tui: cannot run todo: spawn todo ENOENT`. Put `todo` on
  `PATH` (the `cargo install` step), or set `TODO_BIN` to an absolute path.
- A rebuilt `todo` still behaves like the old one. Run `todo daemon stop`.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT)
at your option.
