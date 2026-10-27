#!/usr/bin/env bash
# Smoke test: auto-spawn, a TS watcher over Subscribe, the single-daemon lock
# under parallel --detach, the long-path socket fallback (P0), parsed capture
# and the capture spool round trip (P1 M1), undo/redo of a capture (P1 M2),
# `todo status` and the seeded Today view (P1 M3), and the TUI driven in a
# private tmux server (P1 M4; skipped without tmux).
# Usage: scripts/smoke.sh   (TODO_BIN overrides the binary; default: cargo build;
# TODO_TUI_ENTRY the TUI bundle; default: pnpm -C tui build)
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"

if [[ -z "${TODO_BIN:-}" ]]; then
  cargo build -q -p todo-cli
  TODO_BIN="$repo/target/debug/todo"
fi
export TODO_BIN

root=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/todo-smoke.XXXXXX")" && pwd)
homes=()
watcher_pid=
# The private tmux server's socket directory (step 8), kept short: a unix
# socket path must fit in about 100 bytes.
tmux_dir=

sha8() {
  if command -v sha256sum >/dev/null; then printf '%s' "$1" | sha256sum | cut -c1-8
  else printf '%s' "$1" | shasum -a 256 | cut -c1-8; fi
}

# Pids with data/daemon.lock open, one per line.
lock_holder_pids() {
  local lock=$1/data/daemon.lock
  [[ -e $lock ]] || return 0
  if command -v lsof >/dev/null; then lsof -t -- "$lock" 2>/dev/null || true
  else fuser "$lock" 2>/dev/null | tr -s ' ' '\n' | grep . || true; fi
}

lock_holders() { lock_holder_pids "$1" | sort -u | wc -l | tr -d ' '; }

cleanup() {
  local status=$?
  [[ -n $watcher_pid ]] && kill "$watcher_pid" 2>/dev/null || true
  if [[ -n $tmux_dir ]]; then
    TMUX_TMPDIR=$tmux_dir tmux -L todo-smoke kill-server 2>/dev/null || true
    rm -rf "$tmux_dir"
  fi
  for h in "${homes[@]}"; do
    TODO_TUI_HOME=$h "$TODO_BIN" daemon stop >/dev/null 2>&1 || true
    # The lock file keeps the last daemon's pid after it exits; only kill a
    # pid that still holds the lock, never a recycled one.
    pid=$(head -n1 "$h/data/daemon.lock" 2>/dev/null | tr -dc 0-9 || true)
    if [[ -n $pid ]] && lock_holder_pids "$h" | grep -qx "$pid"; then kill -9 "$pid" 2>/dev/null || true; fi
    rm -rf "/tmp/todo-tui-$(id -u)/h-$(sha8 "$h")"
  done
  rm -rf "$root"
  if (( status == 0 )); then echo "smoke: PASS"; else echo "smoke: FAIL" >&2; fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

fail() { echo "smoke: $*" >&2; exit 1; }
step() { echo "== $*"; }

new_home() {
  local h=$1
  mkdir -p "$h"
  homes+=("$h")
}

# Runs the TS watcher in the background against $TODO_TUI_HOME, waits for READY.
start_watcher() {
  local out=$1
  : >"$out"
  (cd "$repo/packages/client" && SMOKE_TIMEOUT_MS=15000 exec corepack pnpm exec tsx scripts/smoke-watch.ts) \
    >"$out" 2>"$out.err" &
  watcher_pid=$!
  for _ in $(seq 1 150); do
    grep -q '^READY$' "$out" && return 0
    kill -0 "$watcher_pid" 2>/dev/null || { cat "$out.err" >&2; fail "watcher exited before READY"; }
    sleep 0.1
  done
  fail "watcher never printed READY"
}

# Prints every item as protobuf JSON, one per line.
list_items() {
  (cd "$repo/packages/client" && corepack pnpm exec tsx scripts/smoke-watch.ts --list)
}

# Prints the items the view seeded from $1 lists, in its order, as list_items does.
list_view() {
  (cd "$repo/packages/client" && corepack pnpm exec tsx scripts/smoke-watch.ts --view "$1")
}

wait_watcher_sees() {
  local out=$1 title=$2
  wait "$watcher_pid" || { cat "$out.err" >&2; fail "watcher failed"; }
  watcher_pid=
  grep -q "\"title\":\"$title\"" "$out" || { cat "$out" >&2; fail "watcher did not see '$title'"; }
}

# 1. `todo add` auto-spawns the daemon; the TS watcher sees the next add.
step "auto-spawn and Subscribe"
home=$root/basic
new_home "$home"
export TODO_TUI_HOME=$home
[[ -S $home/run/daemon.sock ]] && fail "socket exists before first add"
out=$("$TODO_BIN" add "first item")
[[ $out =~ ^[0-9A-Z]{8}\ \ first\ item$ ]] || fail "unexpected add output: $out"
[[ -S $home/run/daemon.sock ]] || fail "add did not spawn a daemon"
start_watcher "$root/watch1.out"
"$TODO_BIN" add "seen by watcher" >/dev/null
wait_watcher_sees "$root/watch1.out" "seen by watcher"
"$TODO_BIN" daemon stop >/dev/null
[[ $(lock_holders "$home") == 0 ]] || fail "daemon still holds the lock after stop"

# 2. Five parallel `todo daemon --detach` leave exactly one daemon.
step "parallel --detach"
home=$root/parallel
new_home "$home"
export TODO_TUI_HOME=$home
pids=()
for _ in 1 2 3 4 5; do "$TODO_BIN" daemon --detach >/dev/null 2>&1 & pids+=($!); done
for p in "${pids[@]}"; do wait "$p" || fail "a --detach call failed"; done
sleep 0.3
n=$(lock_holders "$home")
[[ $n == 1 ]] || fail "expected one daemon, found $n"
"$TODO_BIN" add "after race" >/dev/null
"$TODO_BIN" daemon stop >/dev/null

# 3. A TODO_TUI_HOME long enough to push the socket past 100 bytes falls back
#    to /tmp/todo-tui-$UID/h-<sha8>/; both the CLI and the TS client agree.
step "long-path fallback"
long=$root/long
while (( ${#long} < 110 )); do long=$long/abcdefghij; done
new_home "$long"
export TODO_TUI_HOME=$long
"$TODO_BIN" add "long home" >/dev/null
fallback=/tmp/todo-tui-$(id -u)/h-$(sha8 "$long")/daemon.sock
[[ -S $fallback ]] || fail "no socket at $fallback"
[[ -e $long/run/daemon.sock ]] && fail "socket created at the over-long path"
start_watcher "$root/watch2.out"
"$TODO_BIN" add "long watcher" >/dev/null
wait_watcher_sees "$root/watch2.out" "long watcher"
"$TODO_BIN" daemon stop >/dev/null

# 4. `todo add` parses the capture syntax and prints the parsed fields; the TS
#    watcher sees them on the Item.
step "parsed capture"
home=$root/parsed
new_home "$home"
export TODO_TUI_HOME=$home
start_watcher "$root/watch3.out"
out=$("$TODO_BIN" add "call dentist ^2026-10-24 +2 #home")
re='^[0-9A-Z]{8}  call dentist  \+2 \^2026-10-24 #home$'
[[ $out =~ $re ]] || fail "unexpected add output: $out"
wait_watcher_sees "$root/watch3.out" "call dentist"
for field in '"priority":"PRIORITY_HIGH"' '"dueDate":{"year":2026,"month":10,"day":24}' '"labels":["home"]'; do
  grep -qF "$field" "$root/watch3.out" || { cat "$root/watch3.out" >&2; fail "watcher item lacks $field"; }
done
"$TODO_BIN" daemon stop >/dev/null

# 5. With the daemon down and unable to start (a file where its runtime dir
#    belongs), `todo add` spools and succeeds; the next daemon imports it.
step "spool round trip"
home=$root/spool
new_home "$home"
export TODO_TUI_HOME=$home
: >"$home/run"
out=$("$TODO_BIN" add "spooled item ^2026-10-24 #later" 2>"$root/spool.err") \
  || { cat "$root/spool.err" >&2; fail "add failed while the daemon could not start"; }
[[ $out == "spooled  spooled item ^2026-10-24 #later" ]] || fail "unexpected add output: $out"
grep -qF "spooled to $home/data/spool.jsonl" "$root/spool.err" || { cat "$root/spool.err" >&2; fail "add did not say where it spooled"; }
[[ $(wc -l <"$home/data/spool.jsonl" | tr -d ' ') == 1 ]] || fail "expected one spool line"
rm "$home/run"
"$TODO_BIN" daemon --detach >/dev/null
for _ in $(seq 1 50); do [[ -s $home/data/spool.jsonl ]] || break; sleep 0.1; done
[[ -s $home/data/spool.jsonl ]] && fail "the daemon did not import the spool"
list_items >"$root/spool.items" || fail "listing items failed"
[[ $(grep -c '"title":"spooled item"' "$root/spool.items") == 1 ]] || { cat "$root/spool.items" >&2; fail "spooled item not imported exactly once"; }
grep -qF '"labels":["later"]' "$root/spool.items" || { cat "$root/spool.items" >&2; fail "spooled item lost its label"; }
"$TODO_BIN" daemon stop >/dev/null

# 6. `todo undo` soft-deletes the capture, and the TS watcher sees deletedAt;
#    `todo redo` brings it back, and the watcher sees it live again.
step "undo and redo"
home=$root/undo
new_home "$home"
export TODO_TUI_HOME=$home
"$TODO_BIN" add "undo me +2" >/dev/null
start_watcher "$root/watch4.out"
out=$("$TODO_BIN" undo)
[[ $out == 'Undid: capture "undo me"' ]] || fail "unexpected undo output: $out"
wait_watcher_sees "$root/watch4.out" "undo me"
grep -q '"deletedAt":"' "$root/watch4.out" || { cat "$root/watch4.out" >&2; fail "watcher did not see the undone capture deleted"; }
start_watcher "$root/watch5.out"
out=$("$TODO_BIN" redo)
[[ $out == 'Redid: capture "undo me"' ]] || fail "unexpected redo output: $out"
wait_watcher_sees "$root/watch5.out" "undo me"
grep -q '"deletedAt"' "$root/watch5.out" && { cat "$root/watch5.out" >&2; fail "watcher saw the redone capture still deleted"; }
grep -qF '"priority":"PRIORITY_HIGH"' "$root/watch5.out" || { cat "$root/watch5.out" >&2; fail "the redone capture lost its priority"; }
out=$("$TODO_BIN" redo)
[[ $out == "Nothing to redo" ]] || fail "unexpected second redo output: $out"
"$TODO_BIN" daemon stop >/dev/null

# 7. `todo status --short` prints nothing and starts no daemon while none
#    runs, then counts the inbox and what is due; the TS client lists the
#    seeded Today view: overdue first, then by priority, whatever the
#    capture order.
step "status and the Today view"
home=$root/status
new_home "$home"
export TODO_TUI_HOME=$home
out=$("$TODO_BIN" status --short) || fail "status --short failed without a daemon"
[[ -z $out ]] || fail "status --short printed '$out' without a daemon"
[[ -e $home/run || -e $home/data ]] && fail "status --short started a daemon"
"$TODO_BIN" add "due today ^today" >/dev/null
"$TODO_BIN" add "overdue +4 ^2020-01-01" >/dev/null
"$TODO_BIN" add "urgent +1 ^today" >/dev/null
"$TODO_BIN" add "not due ^2999-12-31" >/dev/null
# It prints nothing when the daemon misses its 40 ms deadline, so ask again.
for _ in 1 2 3 4 5; do
  out=$("$TODO_BIN" status --short)
  [[ -n $out ]] && break
done
[[ $out == '📥4 ⏰3' ]] || fail "unexpected status --short output: '$out'"
"$TODO_BIN" status >"$root/status.out" || fail "status failed"
for line in '⏰ Due: 3 (1 overdue)' '  Today: 3'; do
  grep -qxF "$line" "$root/status.out" || { cat "$root/status.out" >&2; fail "status lacks '$line'"; }
done
list_view today >"$root/today.items" || fail "listing the Today view failed"
titles=$(sed -n 's/.*"title":"\([^"]*\)".*/\1/p' "$root/today.items" | paste -sd, -)
[[ $titles == "overdue,urgent,due today" ]] || { cat "$root/today.items" >&2; fail "unexpected Today view: $titles"; }
"$TODO_BIN" daemon stop >/dev/null

# 8. The TUI in a private tmux server, against a scratch daemon it spawns:
#    quick-add with the live preview, x takes the row out of the Inbox and u
#    brings it back, m m x completes two rows in one op and u brings both
#    back, E edits the description in $EDITOR (a script) with the terminal
#    resized meanwhile, ctrl-z without job control comes straight back, q
#    leaves the terminal as it was, and ctrl-z / fg in a job-control shell
#    stop and resume it. The TS client checks the
#    daemon. Frames are polled with capture-pane, not fixed sleeps.
step "the TUI in tmux"
if ! command -v tmux >/dev/null; then
  echo "smoke: tmux not found; skipping the TUI step"
else
  home=$root/tui
  new_home "$home"
  export TODO_TUI_HOME=$home
  if [[ -z "${TODO_TUI_ENTRY:-}" ]]; then
    corepack pnpm --silent -C "$repo/tui" build
    TODO_TUI_ENTRY="$repo/tui/dist/tui.js"
  fi
  export TODO_TUI_ENTRY
  tmux_dir=$(mktemp -d /tmp/todo-smoke-tmux.XXXXXX)
  # No tmux.conf: the user's (plugins, default-terminal, hooks) must not change the run.
  tmx() { TMUX_TMPDIR=$tmux_dir tmux -L todo-smoke -f /dev/null "$@"; }
  screen() { tmx capture-pane -p -t tui; }
  # Waits until the screen matches the extended regex $1 ($2 says what for).
  see() {
    for _ in $(seq 1 100); do
      screen | grep -Eq -- "$1" && return 0
      sleep 0.1
    done
    screen >&2
    fail "the TUI never showed $2"
  }
  keys() { tmx send-keys -t tui "$@"; }
  # E's editor: says it started, waits to be let go (so the terminal can be
  # resized meanwhile), then appends to the description.
  editor=$root/editor.sh
  printf '%s\n' '#!/bin/sh' ": >'$root/editing'" "while [ ! -e '$root/go' ]; do sleep 0.05; done" 'printf "edited by smoke" >>"$1"' >"$editor"
  chmod +x "$editor"
  # `todo` with no command runs the bundle; the line before it and the
  # line after it show whether quitting restored the screen.
  tmx new-session -d -s tui -x 100 -y 20 -e "TODO_TUI_HOME=$home" -e "TODO_BIN=$TODO_BIN" -e "TODO_TUI_ENTRY=$TODO_TUI_ENTRY" \
    -e "VISUAL=$editor" -e "EDITOR=$editor" \
    "echo before-the-tui; \"\$TODO_BIN\"; echo \"the tui exited \$?\"; sleep 600"
  see '● live' "the live connection"
  see 'Inbox zero' "the empty Inbox"

  keys a
  see '^ADD · ' "quick-add"
  keys -l 'smoke item +2 ^fri'
  see '^→ smoke item · \+2 · \^Fri ' "the capture preview"
  keys Enter
  see '^› ○ \+2 +smoke item' "the captured row"
  keys Escape
  see '^NORMAL · ' "normal mode"
  list_items >"$root/tui.items" || fail "listing items failed"
  for field in '"title":"smoke item"' '"priority":"PRIORITY_HIGH"' '"dueDate":{' '"createdVia":"CREATED_VIA_TUI"' '"status":"STATUS_INBOX"'; do
    grep -qF "$field" "$root/tui.items" || { cat "$root/tui.items" >&2; fail "the TUI's capture lacks $field"; }
  done

  keys x
  see '^mark done "smoke item" · u undo' "the x toast"
  see 'Inbox zero' "the row leaving the Inbox"
  # ListItems leaves done Items out unless asked.
  list_items >"$root/tui.items" || fail "listing items failed"
  grep -qF '"title":"smoke item"' "$root/tui.items" && { cat "$root/tui.items" >&2; fail "x did not mark it done"; }
  keys u
  see '^Undid: mark done "smoke item"' "the undo toast"
  see '^› ○ \+2 +smoke item' "the row back in the Inbox"
  list_items >"$root/tui.items" || fail "listing items failed"
  grep -qF '"status":"STATUS_INBOX"' "$root/tui.items" || { cat "$root/tui.items" >&2; fail "u did not bring it back"; }

  # Two marked rows, done in one op, and back with one u.
  keys a
  see '^ADD · ' "quick-add"
  keys -l 'second item'
  keys Enter
  see '^  ○ +second item' "the second row"
  keys Escape
  see '^NORMAL · ' "normal mode"
  keys g g m m
  see '· 2 marked' "two marks"
  keys x
  see '^mark done \(2 items\) · u undo' "the bulk x toast"
  see 'Inbox zero' "both rows leaving the Inbox"
  list_items >"$root/tui.items" || fail "listing items failed"
  grep -qF '"title":"' "$root/tui.items" && { cat "$root/tui.items" >&2; fail "m m x left an Item open"; }
  keys u
  see '^Undid: mark done \(2 items\)' "the bulk undo toast"
  see '^  ○ +second item' "both rows back"
  list_items >"$root/tui.items" || fail "listing items failed"
  [[ $(grep -c '"status":"STATUS_INBOX"' "$root/tui.items") == 2 ]] || { cat "$root/tui.items" >&2; fail "u did not bring both back"; }

  # E on the first row, the terminal resized while the editor runs: the
  # TUI comes back at the new size, and the description is saved.
  keys g g E
  for _ in $(seq 1 100); do [[ -e $root/editing ]] && break; sleep 0.1; done
  [[ -e $root/editing ]] || { screen >&2; fail "E did not start the editor"; }
  tmx resize-window -t tui -x 80 -y 16
  : >"$root/go"
  see '^edit "smoke item" · u undo' "the E toast"
  [[ $(screen | wc -l | tr -d ' ') == 16 ]] || { screen >&2; fail "the TUI did not come back at the new size"; }
  screen | sed -n 15p | grep -q '^NORMAL · ● live' || { screen >&2; fail "the TUI did not redraw at the new size"; }
  list_items >"$root/tui.items" || fail "listing items failed"
  grep -qF '"description":"edited by smoke"' "$root/tui.items" || { cat "$root/tui.items" >&2; fail "E did not save the description"; }

  # ctrl-z with no job control (the session's shell runs the TUI with none):
  # nothing can stop it, so it takes the terminal back at once and says so.
  keys C-z
  see '^Cannot suspend: no job control here' "the ctrl-z toast"
  keys j
  see '^› ○ +second item' "the TUI taking keys after ctrl-z"

  keys q
  see '^the tui exited 0$' "a clean exit"
  # The alternate screen is gone: the shell's lines are back, the TUI's are not.
  screen | grep -qx 'before-the-tui' || { screen >&2; fail "quitting did not restore the screen"; }
  screen | grep -q '● live' && { screen >&2; fail "the TUI's frame stayed on the screen"; }

  # ctrl-z in a shell with job control stops the TUI; fg brings it back,
  # redrawn, with what changed meanwhile.
  tmx new-window -t tui -n job -e "TODO_TUI_HOME=$home" -e "TODO_BIN=$TODO_BIN" -e "TODO_TUI_ENTRY=$TODO_TUI_ENTRY" -e "PS1=$ " \
    "env -u TMUX bash --norc --noprofile -i"
  jscreen() { tmx capture-pane -p -t tui:job; }
  jsee() {
    for _ in $(seq 1 100); do
      jscreen | grep -Eq -- "$1" && return 0
      sleep 0.1
    done
    jscreen >&2
    fail "the job-control shell never showed $2"
  }
  tmx send-keys -t tui:job '"$TODO_BIN"' Enter
  jsee '● live' "the TUI"
  tmx send-keys -t tui:job C-z
  jsee 'Stopped' "the TUI stopped"
  "$TODO_BIN" add "while stopped" >/dev/null
  tmx send-keys -t tui:job fg Enter
  jsee '○ +while stopped' "the TUI resumed with the change"
  jsee '^NORMAL · ● live' "the TUI live again"
  tmx send-keys -t tui:job q
  # The prompt is back, alone on its line (capture-pane drops its trailing space).
  jsee '^\$$' "the shell after the TUI quit"
  tmx kill-server
  rm -rf "$tmux_dir"
  tmux_dir=
  "$TODO_BIN" daemon stop >/dev/null
fi
