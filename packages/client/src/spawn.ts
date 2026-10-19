import { spawn } from "node:child_process";
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
