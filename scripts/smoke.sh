#!/usr/bin/env bash
# Smoke test: auto-spawn, a TS watcher over Subscribe, the single-daemon lock
# under parallel --detach, the long-path socket fallback (P0), parsed capture
# and the capture spool round trip (P1 M1).
# Usage: scripts/smoke.sh   (TODO_BIN overrides the binary; default: cargo build)
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
