import { dirname } from "node:path";
import { type Client, type Transport, createClient } from "@connectrpc/connect";
import { ItemService } from "./gen/todo/v1/item_service_pb.js";
import { ClientKind, type GetServerInfoResponse, SystemService } from "./gen/todo/v1/system_service_pb.js";
import { WatchService } from "./gen/todo/v1/watch_service_pb.js";
import { LiveStore, type LiveStoreOptions } from "./live-store.js";
import { defaultSocketPath } from "./paths.js";
import { spawnDaemon, withAutoSpawn } from "./spawn.js";
import { type TodoTransport, type TodoTransportOptions, UnsafeRuntimeDirError, checkRuntimeDir, createTodoTransport } from "./transport.js";

/** The api_minor this client was built against. */
export const CLIENT_API_MINOR = 0;
export const CLIENT_VERSION = "0.1.0";

export interface TodoClients {
  system: Client<typeof SystemService>;
  items: Client<typeof ItemService>;
  watch: Client<typeof WatchService>;
}

export interface TodoConnection extends TodoClients {
  transport: TodoTransport;
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
  ) {
    super(`daemon ${serverInfo.daemonVersion} has api_minor ${serverInfo.apiMinor}, client needs ${requiredApiMinor}`);
    this.name = "DaemonTooOldError";
  }
}

export function createClients(transport: Transport): TodoClients {
  return {
    system: createClient(SystemService, transport),
    items: createClient(ItemService, transport),
    watch: createClient(WatchService, transport),
  };
}

/** Dials the daemon (spawning it if needed) and performs the GetServerInfo handshake (4.2). */
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
  } catch (err) {
    transport.close();
    throw err;
  }
  if (serverInfo.apiMinor < requiredApiMinor) {
    transport.close();
    throw new DaemonTooOldError(serverInfo, requiredApiMinor);
  }
  return {
    ...clients,
    transport,
    serverInfo,
    ensureDaemon: () => (opts.autoSpawn === false ? Promise.resolve() : spawnDaemon(spawnOpts)),
    close: () => transport.close(),
  };
}

/** A started LiveStore on this connection that respawns the daemon if it goes away. */
export function createLiveStore(conn: TodoConnection, opts: Omit<LiveStoreOptions, "transport"> = {}): LiveStore {
  const store = new LiveStore({ ensureDaemon: conn.ensureDaemon, ...opts, transport: conn.transport });
  store.start();
  return store;
}
