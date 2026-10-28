// ctrl-z (Decision 10): raw mode turns the terminal's ctrl-z into a key, so
// the TUI stops itself as a shell job, the terminal handed back first. A
// process group with no job control (the TUI run as `tmux new-session cmd`
// or `xterm -e todo`: a session of its own, so the group is orphaned) has
// its SIGTSTP discarded by the kernel, and no SIGCONT ever comes: waiting
// for one would leave the TUI with the terminal handed back for good.
// E's editor runs with the terminal in cooked mode, so a ctrl-c meant for
// an editor that leaves it so (`code --wait`) reaches this process too.
// No imports, so that a test can run it in a session of its own.

/**
 * How long after the SIGTSTP to wait for SIGCONT. A stop takes effect
 * before kill returns, so after fg the SIGCONT is a loop turn away.
 */
export const STOP_GRACE_MS = 250;

/** Stops this process as a job; resolves on fg with true, or with false when nothing stopped it. */
export function stopSelf(graceMs = STOP_GRACE_MS): Promise<boolean> {
  return new Promise((resolve) => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const resumed = () => {
      clearTimeout(timer);
      resolve(true);
    };
    process.once("SIGCONT", resumed);
    process.kill(process.pid, "SIGTSTP");
    timer = setTimeout(() => {
      process.off("SIGCONT", resumed);
      resolve(false);
    }, graceMs);
  });
}

/**
 * Runs `run` (E's spawnSync of the editor) with SIGINT and SIGQUIT ignored
 * here, as git does while its editor runs: the editor gets them, and the
 * TUI carries on and takes the terminal back. Resolves once a signal that
 * came meanwhile has been handled, by the no-op: a loop turn after `run`
 * returns, as the loop reads signals in its poll phase.
 */
export async function withoutInterrupts<T>(run: () => T): Promise<T> {
  const ignore = () => {};
  const signals = ["SIGINT", "SIGQUIT"] as const;
  for (const sig of signals) process.on(sig, ignore);
  try {
    return run();
  } finally {
    // This turn's check phase, then the next turn's poll (the signal) and check.
    for (let turn = 0; turn < 2; turn++) await new Promise((resolve) => setImmediate(resolve));
    for (const sig of signals) process.off(sig, ignore);
  }
}
