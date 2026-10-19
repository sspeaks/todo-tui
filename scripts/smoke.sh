#!/usr/bin/env bash
# P0 smoke test (plan S0.9): auto-spawn, a TS watcher over Subscribe, the
# single-daemon lock under parallel --detach, and the long-path socket fallback.
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
