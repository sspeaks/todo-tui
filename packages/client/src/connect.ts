import { dirname } from "node:path";
import { Code, ConnectError, type Client, type Transport, createClient } from "@connectrpc/connect";
import { ItemService } from "./gen/todo/v1/item_service_pb.js";
import { SyncService } from "./gen/todo/v1/sync_service_pb.js";
import { ClientKind, type GetServerInfoResponse, SystemService } from "./gen/todo/v1/system_service_pb.js";
import { ViewService } from "./gen/todo/v1/view_service_pb.js";
import { WatchService } from "./gen/todo/v1/watch_service_pb.js";
import { type TodoClients, createClients } from "./clients.js";
import { connectRemote } from "./remote.js";
import { LiveStore, type LiveStoreOptions } from "./live-store.js";
import { errorReason } from "./mutations.js";
import { defaultSocketPath, envFromProcess, resolvePaths } from "./paths.js";
import { DaemonTooOldError, isConnectionFailure } from "./errors.js";
import { readDaemonPid, spawnDaemon, waitForDaemonExit, withAutoSpawn } from "./spawn.js";
import { type TodoTransport, type TodoTransportOptions, UnsafeRuntimeDirError, checkRuntimeDir, createTodoTransport } from "./transport.js";

/** The api_minor this client was built against. */
export const CLIENT_API_MINOR = 23;
export const CLIENT_VERSION = "0.1.0";

export { type TodoClients, createClients } from "./clients.js";
export { DaemonTooOldError } from "./errors.js";

export interface TodoConnection extends TodoClients {
  transport: TodoTransport;
  /** From the latest handshake; each new connection to the daemon repeats it. */
  serverInfo: GetServerInfoResponse;
  /** Spawns the daemon again; LiveStore uses it to recover from a daemon exit. */
  ensureDaemon: () => Promise<void>;
  close(): void;
}

export interface ConnectOptions extends TodoTransportOptions {
  /** Defaults to the socket resolved from `env` (paths.ts). */
  socketPath?: string;
  env?: NodeJS.ProcessEnv;
  clientKind?: ClientKind;
  clientVersion?: string;
  requiredApiMinor?: number;
  /** Spawn `todo daemon --detach` when nothing answers (default true). */
  autoSpawn?: boolean;
  /** Binary to spawn; defaults to $TODO_BIN, then `todo` on PATH. */
  todoBin?: string;
  spawnBudgetMs?: number;
}

/** The daemon is too old and refused the upgrade restart: other interactive clients are connected (4.2). */
export class DaemonRestartRefusedError extends DaemonTooOldError {
  constructor(serverInfo: GetServerInfoResponse, requiredApiMinor: number, options?: ErrorOptions) {
    super(serverInfo, requiredApiMinor, options);
    this.message = `restart required (${this.message}); close other todo sessions first`;
    this.name = "DaemonRestartRefusedError";
  }
}

/** The steps of an upgrade restart; injectable for tests. */
export interface UpgradeRestartSteps {
  /** SystemService.Shutdown({ restart: true }) on the old daemon. */
  shutdown: () => Promise<unknown>;
  /** Resolves once the old daemon has exited. */
  waitForExit: () => Promise<void>;
  spawn: () => Promise<void>;
  handshake: () => Promise<GetServerInfoResponse>;
}

function isRestartRefused(err: unknown): boolean {
  return err instanceof ConnectError && err.code === Code.FailedPrecondition && errorReason(err) === "INTERACTIVE_CLIENTS_CONNECTED";
}

/**
 * Upgrade restart (4.2) of a daemon whose api_minor is below
 * `requiredApiMinor`: shuts it down, waits for it to exit, spawns it again
 * and repeats the handshake once. A shutdown that fails because the daemon is
 * already gone counts as done. Returns the new daemon's server info.
 */
export async function upgradeRestart(
  serverInfo: GetServerInfoResponse,
  requiredApiMinor: number,
  steps: UpgradeRestartSteps,
): Promise<GetServerInfoResponse> {
  try {
    await steps.shutdown();
  } catch (err) {
    if (isRestartRefused(err)) throw new DaemonRestartRefusedError(serverInfo, requiredApiMinor, { cause: err });
    if (!isConnectionFailure(err)) throw err;
  }
  await steps.waitForExit();
  await steps.spawn();
  const info = await steps.handshake();
  if (info.apiMinor < requiredApiMinor) throw new DaemonTooOldError(info, requiredApiMinor);
  return info;
}

/**
 * The pid in the daemon lock, read only when `socketPath` is the socket `env`
 * resolves to: a lock elsewhere belongs to some other daemon.
 */
function daemonPidFor(socketPath: string, env: NodeJS.ProcessEnv): number | undefined {
  try {
    const paths = resolvePaths(envFromProcess(env));
    return paths.socket === socketPath ? readDaemonPid(paths.lock) : undefined;
  } catch {
    return undefined;
  }
}

/**
 * Dials the daemon (spawning it if needed) and performs the GetServerInfo
 * handshake (4.2). A daemon older than `requiredApiMinor` gets an upgrade
 * restart unless `autoSpawn` is false. Every later connection (after the
 * daemon restarts, say) handshakes again before its first call, so the
 * daemon keeps counting an interactive client; a daemon that is then too old
 * fails those calls with a DaemonTooOldError cause.
 */
export async function connect(opts: ConnectOptions = {}): Promise<TodoConnection> {
  const env = opts.env ?? process.env;
  const socketPath = opts.socketPath ?? defaultSocketPath(env);
  try {
    checkRuntimeDir(dirname(socketPath));
  } catch (err) {
    // Missing is fine (the daemon creates it); anything unsafe is fatal.
    if (err instanceof UnsafeRuntimeDirError) throw err;
  }
  // TODO_NO_SPAWN: a service starts the daemon (the NixOS module's); none of our own.
  const noSpawn = !!env.TODO_NO_SPAWN && env.TODO_NO_SPAWN !== "0";
  opts = noSpawn ? { ...opts, autoSpawn: false } : opts;
  const transport = createTodoTransport(socketPath, opts);
  const clients = createClients(transport);
  const requiredApiMinor = opts.requiredApiMinor ?? CLIENT_API_MINOR;
  const spawnOpts = {
    env,
    ...(opts.todoBin !== undefined && { bin: opts.todoBin }),
  };
  const handshake = () =>
    clients.system.getServerInfo(
      {
        clientKind: opts.clientKind ?? ClientKind.INTERACTIVE,
        clientVersion: opts.clientVersion ?? CLIENT_VERSION,
        requiredApiMinor,
      },
      { timeoutMs: 2_000 },
    );
  let serverInfo: GetServerInfoResponse;
  try {
    serverInfo = await withAutoSpawn(handshake, {
      ...spawnOpts,
      ...(opts.autoSpawn !== undefined && { autoSpawn: opts.autoSpawn }),
      ...(opts.spawnBudgetMs !== undefined && { budgetMs: opts.spawnBudgetMs }),
    });
    if (serverInfo.apiMinor < requiredApiMinor) {
      if (opts.autoSpawn === false) throw new DaemonTooOldError(serverInfo, requiredApiMinor);
      const pid = daemonPidFor(socketPath, env);
      serverInfo = await upgradeRestart(serverInfo, requiredApiMinor, {
        shutdown: () => clients.system.shutdown({ restart: true }, { timeoutMs: 15_000 }),
        waitForExit: () => {
          // Let go of the old daemon; the next handshake dials afresh.
          transport.close();
          return waitForDaemonExit(pid, socketPath);
        },
        spawn: () => spawnDaemon(spawnOpts),
        handshake,
      });
    }
  } catch (err) {
    transport.close();
    throw err;
  }
  const conn: TodoConnection = {
    ...clients,
    transport,
    serverInfo,
    ensureDaemon: () => (opts.autoSpawn === false ? Promise.resolve() : spawnDaemon(spawnOpts)),
    close: () => transport.close(),
  };
  transport.greetEachConnection(`/${SystemService.typeName}/${SystemService.method.getServerInfo.name}`, async () => {
    const info = await handshake();
    if (info.apiMinor < requiredApiMinor) throw new DaemonTooOldError(info, requiredApiMinor);
    conn.serverInfo = info;
  });
  return conn;
}

/** A started LiveStore on this connection that respawns the daemon if it goes away. */
export function createLiveStore(conn: TodoConnection, opts: Omit<LiveStoreOptions, "transport"> = {}): LiveStore {
  const store = new LiveStore({ ensureDaemon: conn.ensureDaemon, ...opts, transport: conn.transport });
  store.start();
  return store;
}

/** A daemon, local or elsewhere (ADR-0008), as the TUI and MCP use one. */
export interface AnyConnection extends TodoClients {
  transport: Transport;
  serverInfo: GetServerInfoResponse;
  /** Spawns the local daemon again; nothing for a remote one. */
  ensureDaemon: () => Promise<void>;
  close(): void;
  /** The daemon is elsewhere. */
  remote: boolean;
}

/**
 * The daemon this machine's clients use (ADR-0008): the one elsewhere when
 * `TODO_REMOTE` and `TODO_REMOTE_TOKEN` are set (`todo` resolves them from
 * `[client] remote` before it starts the TUI or MCP), else the local one
 * through connect(). A remote daemon is never spawned or restarted.
 */
export async function connectAny(opts: ConnectOptions = {}): Promise<AnyConnection> {
  const env = opts.env ?? process.env;
  const url = env.TODO_REMOTE?.trim();
  if (url) {
    const token = env.TODO_REMOTE_TOKEN?.trim();
    if (!token) throw new Error("TODO_REMOTE is set without TODO_REMOTE_TOKEN");
    const remote = await connectRemote({
      baseUrl: url,
      token,
      clientKind: opts.clientKind ?? ClientKind.INTERACTIVE,
      clientVersion: opts.clientVersion ?? CLIENT_VERSION,
      requiredApiMinor: opts.requiredApiMinor ?? CLIENT_API_MINOR,
    });
    return { ...remote, ensureDaemon: () => Promise.resolve(), close: () => {}, remote: true };
  }
  return { ...(await connect(opts)), remote: false };
}
