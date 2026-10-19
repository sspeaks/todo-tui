import { lstatSync } from "node:fs";
import { Socket, connect as netConnect } from "node:net";
import { dirname } from "node:path";
import type { Transport } from "@connectrpc/connect";
import { Http2SessionManager, createGrpcTransport } from "@connectrpc/connect-node";

export interface TodoTransportOptions {
  /** HTTP/2 keepalive PING interval; detects a dead daemon on idle streams. */
  pingIntervalMs?: number;
  pingTimeoutMs?: number;
  defaultTimeoutMs?: number;
}

export interface TodoTransport extends Transport {
  readonly socketPath: string;
  /** Closes the HTTP/2 session and every open stream. A later call re-dials. */
  close(): void;
}

export class UnsafeRuntimeDirError extends Error {
  constructor(
    readonly dir: string,
    why: string,
  ) {
    super(`refusing to use ${dir}: it ${why}`);
    this.name = "UnsafeRuntimeDirError";
  }
}

/**
 * The directories that keep other users away from the socket: `runtime` and,
 * under the D8 fallback `/tmp/todo-tui-$UID/h-<sha8>`, its parent.
 * Mirrors todo_daemon::client::guarded_dirs.
 */
export function guardedDirs(runtime: string, uid = process.getuid?.() ?? -1): string[] {
  const parent = dirname(runtime);
  return parent === `/tmp/todo-tui-${uid}` ? [runtime, parent] : [runtime];
}

/**
 * Throws unless each of guardedDirs(runtime) is a real directory (not a
 * symlink) owned by us with no group/other permission bits. A missing
 * directory throws ENOENT, which reads as "no daemon yet".
 * Mirrors todo_daemon::client::check_runtime_dir.
 */
export function checkRuntimeDir(runtime: string): void {
  const uid = process.getuid?.();
  if (uid === undefined) return;
  for (const dir of guardedDirs(runtime, uid)) {
    const st = lstatSync(dir);
    if (!st.isDirectory()) throw new UnsafeRuntimeDirError(dir, "is not a directory");
    if (st.uid !== uid) throw new UnsafeRuntimeDirError(dir, "is owned by another user");
    if ((st.mode & 0o077) !== 0) {
      throw new UnsafeRuntimeDirError(dir, `is accessible by other users (mode ${(st.mode & 0o777).toString(8)})`);
    }
  }
}

/** Dials `socketPath` after checking its directory (a failed check throws). */
function dial(socketPath: string): Socket {
  checkRuntimeDir(dirname(socketPath));
  return netConnect(socketPath);
}

/** gRPC over HTTP/2 over the daemon's Unix socket; `:authority` is localhost (4.2). */
export function createTodoTransport(socketPath: string, opts: TodoTransportOptions = {}): TodoTransport {
  const sessionManager = new Http2SessionManager(
    "http://localhost",
    {
      pingIntervalMs: opts.pingIntervalMs ?? 15_000,
      pingTimeoutMs: opts.pingTimeoutMs ?? 5_000,
      pingIdleConnection: false,
    },
    { createConnection: () => dial(socketPath) },
  );
  const transport = createGrpcTransport({
    baseUrl: "http://localhost",
    sessionManager,
    ...(opts.defaultTimeoutMs !== undefined && { defaultTimeoutMs: opts.defaultTimeoutMs }),
  });
  return {
    unary: transport.unary.bind(transport),
    stream: transport.stream.bind(transport),
    socketPath,
    close: () => sessionManager.abort(),
  };
}
