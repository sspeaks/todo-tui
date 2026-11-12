#!/usr/bin/env bash
# Smoke test: auto-spawn, a TS watcher over Subscribe, the single-daemon lock
# under parallel --detach, the long-path socket fallback (P0), parsed capture
# and the capture spool round trip (P1 M1), undo/redo of a capture (P1 M2),
# `todo status` and the seeded Today view (P1 M3), the TUI driven in a
# private tmux server (P1 M4; skipped without tmux), and x in the TUI on an
# @daily capture making the next instance (P1 M5), and :view new with query
# text (P1 M6), and Jira sync, read-only, against a fake Data Center (P2).
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
# The fake Jira of step 9.
jira_pid=
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
  [[ -n $jira_pid ]] && kill "$jira_pid" 2>/dev/null || true
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
out=$("$TODO_BIN" add "call dentist ^2026-10-24 +2 %high #home")
re='^[0-9A-Z]{8}  call dentist  \+2 %high \^2026-10-24 #home$'
[[ $out =~ $re ]] || fail "unexpected add output: $out"
wait_watcher_sees "$root/watch3.out" "call dentist"
for field in '"priority":"PRIORITY_HIGH"' '"energy":"ENERGY_HIGH"' '"dueDate":{"year":2026,"month":10,"day":24}' '"labels":["home"]'; do
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

# 7b. Claude refines on request (ADR-0010), through a stub `claude` that
#     prints the CLI's JSON envelope: never a real call. todo refine shows the
#     proposal, and todo assistant usage what it cost.
step "refine with a stub Claude"
home=$root/refine
new_home "$home"
export TODO_TUI_HOME=$home
stub=$root/claude-stub
printf '%s\n' '#!/bin/sh' 'cat >/dev/null' \
  "printf '%s' '{\"is_error\":false,\"duration_ms\":900,\"total_cost_usd\":0.0003,\"usage\":{\"input_tokens\":200,\"output_tokens\":30},\"structured_output\":{\"rationale\":\"A follow-up, due Friday.\",\"title\":\"Follow up with Dana on the login test\",\"waiting_on\":\"Dana\"}}'" >"$stub"
chmod +x "$stub"
printf '[assistant]\nprovider = "claude_code"\nclaude_bin = "%s"\n' "$stub" >"$home/config.toml"
id=$("$TODO_BIN" add "ping dana re login test" | awk '{print $1}')
out=$("$TODO_BIN" refine "$id") || fail "refine failed: $out"
for line in 'A follow-up, due Friday.' '  title  "ping dana re login test" → "Follow up with Dana on the login test"' '  waiting  — → Dana'; do
  grep -qxF -- "$line" <<<"$out" || fail "refine lacks '$line': $out"
done
"$TODO_BIN" assistant usage | grep -q '^1 requests, 230 tokens' || fail "unexpected assistant usage"
"$TODO_BIN" daemon stop >/dev/null

# 8. The TUI in a private tmux server, against a scratch daemon it spawns:
#    quick-add with the live preview, x takes the row out of the Inbox and u
#    brings it back, m m x completes two rows in one op and u brings both
#    back, E edits the description in $EDITOR (a script) with the terminal
#    resized meanwhile, ctrl-z without job control comes straight back, q
#    leaves the terminal as it was, and ctrl-z / fg in a job-control shell
#    stop and resume it, and x on an @daily capture makes the next
#    instance. The TS client checks the daemon. Frames are polled with
#    capture-pane, not fixed sleeps.
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
  keys -l 'smoke item +2 %low ^fri'
  see '^→ smoke item · \+2 · %low · \^Fri ' "the capture preview"
  keys Enter
  see '^› ○ \+2 ▁ smoke item' "the captured row, its energy glyph after the priority"
  keys Escape
  see '^NORMAL · ' "normal mode"
  list_items >"$root/tui.items" || fail "listing items failed"
  for field in '"title":"smoke item"' '"priority":"PRIORITY_HIGH"' '"energy":"ENERGY_LOW"' '"dueDate":{' '"createdVia":"CREATED_VIA_TUI"' '"status":"STATUS_INBOX"'; do
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
  see '^› ○ \+2 ▁ smoke item' "the row back in the Inbox"
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

  # :view new with a query: ParseQuery and CreateView on the real daemon;
  # the tab appears with its count, and the TUI goes to it.
  keys :
  keys -l 'view new Urgent priority>=high'
  keys Enter
  see '^Created view “Urgent”' "the :view new toast"
  see 'Urgent 1' "the new tab with its count"
  see '^› ○ \+2 ▁ smoke item' "the high-priority row in the new tab"
  # A query with an error keeps the prompt open, the error said under it.
  keys :
  keys -l 'view new Bad dew:1'
  keys Enter
  see 'col 1: unknown field "dew"' "the query error"
  keys Escape
  see '^NORMAL · ' "normal mode"

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

  # x on an @daily capture, in a scratch home of its own: the toast says
  # when the next instance is and the row leaves the Inbox. The instance is
  # deferred to its day, so ListItems leaves it out (as it does the done
  # one); :search finds both, and the instance's detail shows where it
  # came from and its Repeats line. Due tomorrow, so that the next one is
  # after today though midnight passes between the add and the x.
  rhome=$root/recur
  new_home "$rhome"
  out=$(TODO_TUI_HOME=$rhome "$TODO_BIN" add "smoke daily ^tomorrow @daily")
  re='^[0-9A-Z]{8}  smoke daily  \^[0-9]{4}-[0-9]{2}-[0-9]{2} @daily$'
  [[ $out =~ $re ]] || fail "unexpected add output: $out"
  tmx new-window -t tui -n recur -e "TODO_TUI_HOME=$rhome" -e "TODO_BIN=$TODO_BIN" -e "TODO_TUI_ENTRY=$TODO_TUI_ENTRY" \
    "\"\$TODO_BIN\"; sleep 600"
  tmx resize-window -t tui:recur -x 110 -y 24
  rscreen() { tmx capture-pane -p -t tui:recur; }
  rsee() {
    for _ in $(seq 1 100); do
      rscreen | grep -Eq -- "$1" && return 0
      sleep 0.1
    done
    rscreen >&2
    fail "the TUI never showed $2"
  }
  rsee '^NORMAL · ● live' "the TUI live"
  rsee '^› ○ +smoke daily .*🔁' "the @daily capture in the Inbox"
  tmx send-keys -t tui:recur x
  # The year shows only when it is not this year's.
  rsee '^mark done "smoke daily" · next [A-Z][a-z]{2} [A-Z][a-z]{2} [0-9]{1,2}( [0-9]{4})? · u undo' "the x toast with the next date"
  rsee 'Inbox zero' "the row leaving the Inbox"
  TODO_TUI_HOME=$rhome list_items >"$root/recur.items" || fail "listing items failed"
  grep -qF '"title":"smoke daily"' "$root/recur.items" && { cat "$root/recur.items" >&2; fail "x left an open 'smoke daily' showing"; }
  tmx send-keys -t tui:recur :
  tmx send-keys -t tui:recur -l 'search smoke daily'
  tmx send-keys -t tui:recur Enter
  rsee '^[› ] ● +smoke daily ' "the done Item in the search"
  rsee '^[› ] ○ +smoke daily .*⏸.*🔁' "the deferred next instance in the search"
  rscreen | grep -q '^› ●' && tmx send-keys -t tui:recur j
  rsee '^› ○ +smoke daily ' "the next instance selected"
  rsee 'Created +[0-9-]+ [0-9:]+ via recurrence' "the instance created via recurrence"
  rsee 'Repeats +every day · this one ' "the instance's Repeats line"
  tmx send-keys -t tui:recur q

  # 9. Jira sync (P2): a fake Data Center on 127.0.0.1 with two issues;
  #    `todo sync now` imports them, `todo sync status` says ok, and the
  #    TUI shows ⇅ in the status bar and the key in the detail. Then a
  #    push (ADR-0007): x on the to-do issue reaches the fake as a close
  #    with resolution Done, and the next pull reads it back done; E on the
  #    other edits its description, which reaches the fake as wiki markup.
  if command -v python3 >/dev/null; then
    jhome=$root/jira
    new_home "$jhome"
    python3 -I "$repo/scripts/fake-jira.py" smoke-pat "$root/jira.writes" >"$root/jira.port" &
    jira_pid=$!
    for _ in $(seq 1 50); do [[ -s $root/jira.port ]] && break; sleep 0.1; done
    port=$(head -n1 "$root/jira.port")
    [[ -n $port ]] || fail "the fake Jira did not start"
    printf 'smoke-pat' >"$jhome/jira.token"
    chmod 600 "$jhome/jira.token"
    printf '[[instances]]\nid = "work-jira"\nkind = "jira-dc"\nbase_url = "http://127.0.0.1:%s"\ncredential = "file:%s"\nscopes = [{ name = "mine", query = "assignee = currentUser()" }]\npush = true\npromote_default = { project = "PLAT", issue_type = "Task" }\n' \
      "$port" "$jhome/jira.token" >"$jhome/config.toml"
    out=$(TODO_TUI_HOME=$jhome "$TODO_BIN" sync check) || fail "sync check failed: $out"
    [[ $out == "work-jira: ok as Dana Example (JIRAUSER10100), time zone UTC" ]] || fail "unexpected sync check: $out"
    out=$(TODO_TUI_HOME=$jhome "$TODO_BIN" sync now) || fail "sync now failed: $out"
    [[ $out == "work-jira: ok · 2 items · last pull just now" ]] || fail "unexpected sync now: $out"
    TODO_TUI_HOME=$jhome "$TODO_BIN" sync status | grep -q '^work-jira: ok · 2 items' || fail "sync status is not ok"
    TODO_TUI_HOME=$jhome list_items >"$root/jira.items" || fail "listing items failed"
    grep -qF '"key":"PLAT-1"' "$root/jira.items" || { cat "$root/jira.items" >&2; fail "PLAT-1 was not imported"; }
    # E's editor here marks the first list item's line, as a person would.
    jeditor=$root/jira-editor.sh
    printf '%s\n' '#!/bin/sh' "sed -i.bak 's/^- weekly \`totals\`\$/- weekly \`totals\` **now**/' \"\$1\"" >"$jeditor"
    chmod +x "$jeditor"
    tmx new-window -t tui -n jira -e "TODO_TUI_HOME=$jhome" -e "TODO_BIN=$TODO_BIN" -e "TODO_TUI_ENTRY=$TODO_TUI_ENTRY" \
      -e "VISUAL=$jeditor" -e "EDITOR=$jeditor" \
      "\"\$TODO_BIN\"; sleep 600"
    tmx resize-window -t tui:jira -x 130 -y 30
    kscreen() { tmx capture-pane -p -t tui:jira; }
    ksee() {
      for _ in $(seq 1 100); do
        kscreen | grep -Eq -- "$1" && return 0
        sleep 0.1
      done
      kscreen >&2
      fail "the TUI never showed $2"
    }
    ksee '^NORMAL · ● live · ⇅ work-jira (now|[0-9]+m)' "the sync state in the status bar"
    # To Do and In Progress both land in Next (design 5.4); their order is
    # the import's.
    tmx send-keys -t tui:jira 3
    ksee '^[› ] ○ .*smoke jira login fix +PLAT-1 ' "the to-do issue in Next"
    ksee '^[› ] ◐ .*smoke jira report +PLAT-2 ' "the in-progress issue in Next"
    ksee 'Jira +PLAT-[12] ·' "the selected issue's key in the detail"
    # x on the to-do issue (PLAT-1) closes it in Jira as Done.
    kscreen | grep -Eq '^› ○ .*PLAT-1 ' || tmx send-keys -t tui:jira j
    ksee '^› ○ .*smoke jira login fix +PLAT-1 ' "PLAT-1 selected"
    tmx send-keys -t tui:jira x
    for _ in $(seq 1 100); do
      grep -q '"to": "Closed"' "$root/jira.writes" 2>/dev/null && break
      sleep 0.1
    done
    grep -qF '{"method": "POST", "key": "PLAT-1", "to": "Closed", "resolution": "Done"}' "$root/jira.writes" ||
      { cat "$root/jira.writes" >&2 2>/dev/null; fail "x on PLAT-1 did not reach the fake Jira as Closed with Done"; }
    out=$(TODO_TUI_HOME=$jhome "$TODO_BIN" sync now) || fail "sync now after the push failed: $out"
    (cd "$repo/packages/client" && TODO_TUI_HOME=$jhome corepack pnpm exec tsx scripts/smoke-watch.ts --list --all) >"$root/jira.after" ||
      fail "listing items failed"
    grep -F '"key":"PLAT-1"' "$root/jira.after" | grep -qF '"status":"STATUS_DONE"' ||
      { cat "$root/jira.after" >&2; fail "PLAT-1 did not read back done"; }
    grep -F '"key":"PLAT-1"' "$root/jira.after" | grep -qF '"remoteStatusName":"Closed"' ||
      { cat "$root/jira.after" >&2; fail "PLAT-1 did not read back Closed"; }
    # E on PLAT-2 edits its description as Markdown; it reaches the fake as
    # wiki markup, the colour it holds kept (ADR-0009).
    tmx send-keys -t tui:jira k
    ksee '^› ◐ .*smoke jira report +PLAT-2 ' "PLAT-2 selected"
    tmx send-keys -t tui:jira E
    for _ in $(seq 1 100); do
      grep -q '"description"' "$root/jira.writes" 2>/dev/null && break
      sleep 0.1
    done
    grep -F '"description"' "$root/jira.writes" |
      grep -qF '"h2. Report\n* weekly {{totals}} *now*\n{color:#888}footer{color}"' ||
      { cat "$root/jira.writes" >&2 2>/dev/null; fail "E on PLAT-2 did not reach the fake Jira as wiki markup"; }
    out=$(TODO_TUI_HOME=$jhome "$TODO_BIN" sync now) || fail "sync now after the description push failed: $out"
    out=$(TODO_TUI_HOME=$jhome "$TODO_BIN" sync outbox) || fail "sync outbox failed: $out"
    [[ $out == "nothing waits to reach Jira" ]] || fail "unexpected sync outbox: $out"
    tmx send-keys -t tui:jira S
    ksee 'Nothing to review' "an empty Suggestions pane"
    tmx send-keys -t tui:jira Escape
    tmx send-keys -t tui:jira q
    # Promote and attach (design 5.7, 3.3): a local todo becomes PLAT-10 in
    # the fake, and another is linked to a teammate's OPS-9, outside the
    # scope, taking its title.
    pid=$(TODO_TUI_HOME=$jhome "$TODO_BIN" add "smoke promote me" | awk '{print $1}')
    out=$(TODO_TUI_HOME=$jhome "$TODO_BIN" promote "$pid") || fail "promote failed: $out"
    [[ $out == "created PLAT-10  http://127.0.0.1:$port/browse/PLAT-10" ]] || fail "unexpected promote: $out"
    grep -qF '{"method": "POST", "key": "PLAT-10", "created": "smoke promote me"}' "$root/jira.writes" ||
      { cat "$root/jira.writes" >&2; fail "promote did not reach the fake Jira"; }
    lid=$(TODO_TUI_HOME=$jhome "$TODO_BIN" add "smoke link me" | awk '{print $1}')
    out=$(TODO_TUI_HOME=$jhome "$TODO_BIN" link "$lid" ops-9) || fail "link failed: $out"
    grep -qF "linked to OPS-9" <<<"$out" || fail "unexpected link: $out"
    grep -qF "wait in the TUI (S)" <<<"$out" || fail "link did not offer the todo's title back: $out"
    kill "$jira_pid" 2>/dev/null || true
    TODO_TUI_HOME=$jhome "$TODO_BIN" daemon stop >/dev/null
  else
    echo "smoke: python3 not found; skipping the Jira step"
  fi
  tmx kill-server
  rm -rf "$tmux_dir"
  tmux_dir=
  "$TODO_BIN" daemon stop >/dev/null
  TODO_TUI_HOME=$rhome "$TODO_BIN" daemon stop >/dev/null
fi
