# todo-tui

A keyboard-driven, local-first todo manager for the terminal. A Rust daemon owns all
state in SQLite and serves a gRPC API (`todo.v1`) over a Unix socket; an Ink TUI, the
`todo` CLI and an MCP server are clients. See [docs/design.md](docs/design.md).

Status: P1 in progress. Milestone M1 (capture) is done: `todo add` with inline
tokens, batch capture from stdin, and an offline spool. The TUI is still the minimal
P0 inbox list; the full TUI is milestone M4. There are no CLI commands yet to list,
edit, complete or delete items.

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
Inbox 1
› ○ +2  call dentist
● live · rev 1 · 1 items                                   j/k move · q quit
```

`q` quits the TUI. `todo` runs the bundle that `pnpm -C tui build` made in this
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

The daemon imports the spool when it starts and every 30 s while it runs. Relative
dates resolve against the time you captured, not the time of import. Lines it cannot
import are moved to `spool.rejected.jsonl` next to it.

### Seeing your todos: the TUI

There is no `todo list` yet; the TUI is how you see items. `todo` with no
arguments opens it: it runs a single-file bundle of the TUI with `node`, which
must be on `PATH`. Build the bundle once, and again after the TUI changes:

```sh
pnpm -C tui build                      # writes tui/dist/tui.js
todo
```

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

Keys: `j`/`k` or the arrow keys move, `q` or Ctrl-C quits. Captures made elsewhere
appear live at the bottom, briefly highlighted. For now the TUI shows only the Inbox
(items with status `inbox`, oldest first) with status, priority and title. Items
captured with `>next` or `>someday` are stored but not shown anywhere yet, and due
dates, labels and waiting-on are not displayed. Views, quick-add, the detail pane
and editing come with M4.

If no daemon is running, the TUI starts one with `todo daemon --detach`, using
`$TODO_BIN` if it is set and `todo` on `PATH` otherwise. `todo` sets `TODO_BIN` to
itself unless it is already set, so the TUI starts that `todo`'s daemon. Under
`pnpm -C tui dev` with neither, it stops with
`todo-tui: cannot run todo: spawn todo ENOENT`. Give `TODO_BIN` as an absolute path:
`pnpm -C tui dev` runs from `tui/`, so a relative one does not resolve.

### The daemon

You rarely need to manage it: `todo add` and the TUI start it when needed, and only
one daemon runs per data directory.

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
| Database `todo.db`, `daemon.lock`, `spool.jsonl` | `$XDG_DATA_HOME/todo-tui/`, default `~/.local/share/todo-tui/` | `~/Library/Application Support/todo-tui/` |
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
- `packages/client/` — shared TypeScript client (Connect-ES)
- `tui/`, `mcp/` — the Ink TUI and the MCP server (a stub so far)
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

`pnpm test` skips the 9 client tests that run against a real daemon unless
`TODO_BIN` is an absolute path to a built `todo`:
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
