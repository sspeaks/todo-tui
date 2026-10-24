import { dirname } from "node:path";
import { Code, ConnectError, type Client, type Transport, createClient } from "@connectrpc/connect";
import { ErrorInfoSchema } from "./gen/google/rpc/error_details_pb.js";
import { ItemService } from "./gen/todo/v1/item_service_pb.js";
import { ClientKind, type GetServerInfoResponse, SystemService } from "./gen/todo/v1/system_service_pb.js";
import { WatchService } from "./gen/todo/v1/watch_service_pb.js";
import { LiveStore, type LiveStoreOptions } from "./live-store.js";
import { defaultSocketPath, envFromProcess, resolvePaths } from "./paths.js";
import { isConnectionFailure, readDaemonPid, spawnDaemon, waitForDaemonExit, withAutoSpawn } from "./spawn.js";
import { type TodoTransport, type TodoTransportOptions, UnsafeRuntimeDirError, checkRuntimeDir, createTodoTransport } from "./transport.js";

/** The api_minor this client was built against. */
export const CLIENT_API_MINOR = 1;
export const CLIENT_VERSION = "0.1.0";

export interface TodoClients {
  system: Client<typeof SystemService>;
  items: Client<typeof ItemService>;
  watch: Client<typeof WatchService>;
}

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

export class DaemonTooOldError extends Error {
  constructor(
    readonly serverInfo: GetServerInfoResponse,
    readonly requiredApiMinor: number,
    options?: ErrorOptions,
  ) {
    super(`daemon ${serverInfo.daemonVersion} has api_minor ${serverInfo.apiMinor}, client needs ${requiredApiMinor}`, options);
    this.name = "DaemonTooOldError";
  }
}

/** The daemon is too old and refused the upgrade restart: other interactive clients are connected (4.2). */
export class DaemonRestartRefusedError extends DaemonTooOldError {
  constructor(serverInfo: GetServerInfoResponse, requiredApiMinor: number, options?: ErrorOptions) {
    super(serverInfo, requiredApiMinor, options);
    this.message = `restart required (${this.message}); close other todo sessions first`;
    this.name = "DaemonRestartRefusedError";
  }
}

export function createClients(transport: Transport): TodoClients {
  return {
    system: createClient(SystemService, transport),
    items: createClient(ItemService, transport),
    watch: createClient(WatchService, transport),
  };
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
  return (
    err instanceof ConnectError &&
    err.code === Code.FailedPrecondition &&
    err.findDetails(ErrorInfoSchema).some((d) => d.reason === "INTERACTIVE_CLIENTS_CONNECTED")
  );
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
