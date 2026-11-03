# ADR-0005: Config is a snapshot the daemon serves, never stored

- Status: accepted
- Date: 2026-11-02
- Amends: design 2.5 (keymap overrides), 4.3 (SystemService), 4.4 (the `config`
  topic), 8.2 (configuration)

## Context

todo-tui is a terminal todo manager. A Rust **daemon** owns the SQLite database and
serves a gRPC API; the clients (an Ink **TUI**, the `todo` CLI, an MCP server for
Claude Code) are thin. Its design is [`docs/design.md`](../design.md); section numbers
below are that document's. **M6** is the milestone that added config, view editing,
backups and export.

Clients stay current through a **change stream** (design 4.4): every write gets the
next **revision** (a global counter) and a row in the `changes` table, which names a
**topic** (`item`, `view`, `config`, …) and a resource id. A client subscribes from
the revision it last saw (`WatchService.Subscribe`); the daemon replays the rows
after it, then streams new ones, each a `Change` message carrying the resource as it
is now. A client that falls too far behind is told to reload (`ResyncRequired`).

M6 adds `config.toml` (design 8.2). The daemon needs some of it (how many days a
done Item waits before it is archived, backups) and the TUI needs the `[keymap]`
section, which remaps keys. The design said the daemon never writes the file and
that `SystemService` (the daemon's administrative RPCs) has "config read + reload",
and it reserved `Change.resource` field 16 for a `ConfigSnapshot` on topic `config`.
It did not say whether a config change is a stored record, as a view is.

## Decision

**Only the daemon reads the file; clients get a `ConfigSnapshot`.**
`SystemService.GetConfig` serves it, with defaults filled in and a warning for each
problem: a bad value falls back to its default, and nothing stops the daemon, since a
typo in a config file must not take every todo away. `ReloadConfig`, SIGHUP and
`todo config reload` read the file again. `todo config check` runs the daemon's own
reader in the CLI process (the code is one crate, linked into both), so a file can be
checked before any daemon reads it; it changes nothing.

**Config is not stored.** A reload that changes the snapshot records one `config`
row in `changes`, with no copy of the config. When the daemon sends that change, live
or replayed, it fills `Change.resource` field 16 with the config current at that
moment, so a client that missed several reloads sees the latest config on each. The
daemon also records one at every start, since the file may have changed while it was
down. An import records one too: the import moves the revision forward, and a
recorded change at the new revision is what makes connected clients notice the jump
and reload.

**The TUI applies `[keymap]` itself.** An override names a key sequence and the
action it runs (`"g d" = "mark_done"`); the actions are the TUI's. The daemon still
checks each action id and key name, against a copy of the TUI's vocabulary
(`crates/todo-daemon/src/keymap.json`), so `todo config check` catches a typo with no
TUI running; a copy, since the TUI is TypeScript and the daemon Rust, and
`tui/src/keys/keymap.test.ts` fails when the two differ. One check needs the whole
keymap, which only the TUI has: the TUI has no key timeouts, so no sequence may both
run an action and start a longer one (`p` alone cannot run anything, since `p 1` sets
a priority). The TUI applies the overrides as one set, so that moving a key down to
its own prefix (`d = "delete"` with `"d d" = ""`) works in any order, and skips only
the override that would still break that rule, with a warning that `:config` (a TUI
command) shows.

## Alternatives considered

- **Each client reads the TOML.** Simpler for the keymap, but two parsers of one
  file, which can disagree about a value or a warning.
- **Store each config in the database**, as views are. It would give config a history
  that means nothing: the file is the source, and a restored database would bring
  back a config the file no longer has.
- **Watch the file.** Convenient, but it needs a file-watching library and can
  apply a file an editor has half saved. A reload is explicit instead.

## Consequences

- A config change carries the current config, not the one at its revision; a
  replay from before several reloads sends the latest config each time.
- The TUI's keymap and the daemon's vocabulary must stay in step; `keymap.test.ts`
  fails when they do not.
- The daemon records a revision at every start.
