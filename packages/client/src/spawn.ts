import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { connect as netConnect } from "node:net";
import { Code, ConnectError } from "@connectrpc/connect";

/** `$TODO_BIN`, else `todo` from PATH. */
export function resolveTodoBin(vars: NodeJS.ProcessEnv = process.env): string {
  return vars.TODO_BIN || "todo";
}

export class SpawnError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "SpawnError";
  }
}

export interface SpawnOptions {
  bin?: string;
  env?: NodeJS.ProcessEnv;
  timeoutMs?: number;
}

/** Runs `todo daemon --detach` and waits for it to return. */
export function spawnDaemon(opts: SpawnOptions = {}): Promise<void> {
  const env = opts.env ?? process.env;
  const bin = opts.bin ?? resolveTodoBin(env);
  return new Promise((resolve, reject) => {
    const child = spawn(bin, ["daemon", "--detach"], {
      env,
      stdio: ["ignore", "ignore", "pipe"],
      timeout: opts.timeoutMs ?? 5_000,
    });
    let stderr = "";
    child.stderr.setEncoding("utf8").on("data", (d: string) => {
      stderr += d;
    });
    child.on("error", (err) => reject(new SpawnError(`cannot run ${bin}: ${err.message}`, { cause: err })));
    child.on("close", (code, signal) => {
      if (code === 0) resolve();
      else reject(new SpawnError(`${bin} daemon --detach failed (${signal ?? `exit ${code}`}): ${stderr.trim()}`));
    });
  });
}

const CONNECTION_ERRNOS = new Set(["ENOENT", "ECONNREFUSED", "ECONNRESET", "EPIPE", "ENOTSOCK"]);

/** True when an RPC failed because no daemon is listening on the socket. */
export function isConnectionFailure(err: unknown): boolean {
  if (err instanceof ConnectError && err.code === Code.Unavailable) return true;
  for (let e: unknown = err, depth = 0; e != null && depth < 8; depth++) {
    if (typeof e === "object" && "code" in e && CONNECTION_ERRNOS.has(String(e.code))) return true;
    e = typeof e === "object" && "cause" in e ? e.cause : undefined;
  }
  return false;
}

export const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** The pid the running daemon recorded in its lock file (Paths.lock), as the Rust CLI reads it. */
export function readDaemonPid(lockPath: string): number | undefined {
  try {
    const text = readFileSync(lockPath, "utf8").trim();
    const pid = Number(text);
    return /^\d+$/.test(text) && pid > 0 && pid <= 0x7fffffff ? pid : undefined;
  } catch {
    return undefined;
  }
}

/** True while something accepts connections on `socketPath`. */
export function isListening(socketPath: string): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = netConnect(socketPath);
    socket.once("connect", () => {
      socket.destroy();
      resolve(true);
    });
    socket.once("error", () => resolve(false));
  });
}

/**
 * Waits for the daemon to exit: until `pid` is gone when it is known (ESRCH,
 * or EPERM once the pid belongs to another user), otherwise until
 * `socketPath` stops accepting connections. Like the Rust CLI it never takes
 * the lock to probe it. Throws after `timeoutMs`.
 */
export async function waitForDaemonExit(pid: number | undefined, socketPath: string, timeoutMs = 15_000): Promise<void> {
  const running = async () => {
    if (pid === undefined) return isListening(socketPath);
    try {
      process.kill(pid, 0);
      return true;
    } catch {
      return false;
    }
  };
  const deadline = Date.now() + timeoutMs;
  let delay = 20;
  while (await running()) {
    if (Date.now() >= deadline) {
      throw new Error(`daemon${pid === undefined ? "" : ` (pid ${pid})`} still running after ${timeoutMs}ms`);
    }
    await sleep(delay);
    delay = Math.min(delay * 2, 200);
  }
}

/**
 * Calls `attempt` until it stops failing with a connection error, spawning the
 * daemon once after the first such failure. Gives up after `budgetMs` (~2s, 4.2).
 */
export async function withAutoSpawn<T>(
  attempt: () => Promise<T>,
  opts: SpawnOptions & { budgetMs?: number; autoSpawn?: boolean } = {},
): Promise<T> {
  try {
    return await attempt();
  } catch (err) {
    if (!isConnectionFailure(err) || opts.autoSpawn === false) throw err;
  }
  await spawnDaemon(opts);
  const deadline = Date.now() + (opts.budgetMs ?? 2_000);
  let delay = 20;
  for (;;) {
    try {
      return await attempt();
    } catch (err) {
      if (!isConnectionFailure(err) || Date.now() + delay > deadline) throw err;
    }
    await sleep(delay);
    delay = Math.min(delay * 2, 250);
  }
}
