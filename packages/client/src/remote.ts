// The phone app's way in (ADR-0004): gRPC-Web to the daemon's remote
// listener, usually https://<machine>.<tailnet>.ts.net behind `tailscale
// serve`, with the bearer token `todo remote pair` printed. Portable: no
// Node APIs, so it runs in React Native with a streaming fetch (expo/fetch).

import { Code, ConnectError, type Interceptor, type Transport } from "@connectrpc/connect";
import { createGrpcWebTransport } from "@connectrpc/connect-web";
import { type TodoClients, createClients } from "./clients.js";
import { DaemonTooOldError, isConnectionFailure } from "./errors.js";
import { ClientKind, type GetServerInfoResponse } from "./gen/todo/v1/system_service_pb.js";
import { LiveStore, type LiveStoreOptions } from "./live-store.js";

/** The api_minor a remote client needs: 9 brought the listener and MOBILE. */
export const REMOTE_API_MINOR = 9;

export interface RemoteOptions {
  /** The listener's origin, e.g. https://mac.tail1234.ts.net. */
  baseUrl: string;
  /** The token `todo remote pair` printed. */
  token: string;
  /** A fetch that can stream response bodies (expo/fetch on React Native); defaults to the global one. */
  fetch?: typeof globalThis.fetch;
  /** Default ClientKind.MOBILE. */
  clientKind?: ClientKind;
  clientVersion?: string;
  /** Default REMOTE_API_MINOR. */
  requiredApiMinor?: number;
  /** Per-call timeout for unary calls, in ms (default none). */
  timeoutMs?: number;
}

export interface RemoteConnection extends TodoClients {
  transport: Transport;
  baseUrl: string;
  /** From the handshake connectRemote made. */
  serverInfo: GetServerInfoResponse;
}

/** A gRPC-Web transport to the remote listener that sends the bearer token on every call. */
export function createRemoteTransport(opts: RemoteOptions): Transport {
  const auth: Interceptor = (next) => (req) => {
    req.header.set("authorization", `Bearer ${opts.token}`);
    return next(req);
  };
  return createGrpcWebTransport({
    baseUrl: opts.baseUrl.replace(/\/+$/, ""),
    interceptors: [auth],
    ...(opts.fetch !== undefined && { fetch: opts.fetch }),
    ...(opts.timeoutMs !== undefined && { defaultTimeoutMs: opts.timeoutMs }),
  });
}

/**
 * Connects to the remote listener and says hello. Rejects with
 * UNAUTHENTICATED for a bad or revoked token, UNAVAILABLE (or a fetch
 * error) when nothing answers, and DaemonTooOldError when the daemon
 * predates the listener's API. Unlike connect() it cannot spawn or restart
 * the daemon: that is the laptop's to do.
 */
export async function connectRemote(opts: RemoteOptions): Promise<RemoteConnection> {
  const transport = createRemoteTransport(opts);
  const clients = createClients(transport);
  const required = opts.requiredApiMinor ?? REMOTE_API_MINOR;
  const serverInfo = await clients.system.getServerInfo({
    clientKind: opts.clientKind ?? ClientKind.MOBILE,
    clientVersion: opts.clientVersion ?? "0.1.0",
    requiredApiMinor: required,
  });
  if (serverInfo.apiMinor < required) throw new DaemonTooOldError(serverInfo, required);
  return { ...clients, transport, baseUrl: opts.baseUrl, serverInfo };
}

/** A started LiveStore over a remote connection. It reconnects with backoff; there is no daemon to spawn. */
export function createRemoteLiveStore(conn: RemoteConnection, opts: Omit<LiveStoreOptions, "transport" | "ensureDaemon"> = {}): LiveStore {
  const store = new LiveStore({ ...opts, transport: conn.transport });
  store.start();
  return store;
}

/**
 * True when a remote call failed because nothing answered: the laptop is
 * asleep or off the tailnet, `tailscale serve` is not running, or the
 * listener is off. Fetch rejects with a TypeError on a network failure (in
 * Node and React Native alike), which Connect reports as UNKNOWN; a proxy
 * with no backend answers 502/503/504, which it reports as UNAVAILABLE.
 */
export function isUnreachable(err: unknown): boolean {
  if (isConnectionFailure(err)) return true;
  if (!(err instanceof ConnectError)) return err instanceof TypeError;
  if (err.code === Code.Unavailable) return true;
  return err.code === Code.Unknown && err.cause instanceof TypeError;
}

export interface Pairing {
  url: string;
  token: string;
}

/** The scheme and path of the link `todo remote pair` prints. */
export const PAIRING_LINK_PREFIX = "todo-tui://pair?";

/** The server and token in a `todo-tui://pair?url=…&token=…` link, or undefined for anything else. */
export function parsePairingLink(link: string): Pairing | undefined {
  const text = link.trim();
  if (!text.startsWith(PAIRING_LINK_PREFIX)) return undefined;
  const params = new Map<string, string>();
  for (const part of text.slice(PAIRING_LINK_PREFIX.length).split("&")) {
    const eq = part.indexOf("=");
    if (eq <= 0) continue;
    try {
      params.set(part.slice(0, eq), decodeURIComponent(part.slice(eq + 1).replace(/\+/g, " ")));
    } catch {
      return undefined;
    }
  }
  const url = params.get("url")?.replace(/\/+$/, "");
  const token = params.get("token");
  if (!url || !token || !/^https?:\/\/[^/?#\s]+$/.test(url)) return undefined;
  return { url, token };
}
