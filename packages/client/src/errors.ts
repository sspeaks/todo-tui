import { Code, ConnectError } from "@connectrpc/connect";
import type { GetServerInfoResponse } from "./gen/todo/v1/system_service_pb.js";

const CONNECTION_ERRNOS = new Set([
  "ENOENT",
  "ECONNREFUSED",
  "ECONNRESET",
  "EPIPE",
  "ENOTSOCK",
  // The remote listener over fetch (undici): no name, no route, a timeout,
  // a connection dropped mid-call.
  "ENOTFOUND",
  "EAI_AGAIN",
  "ETIMEDOUT",
  "EHOSTUNREACH",
  "ENETUNREACH",
  "UND_ERR_SOCKET",
  "UND_ERR_CONNECT_TIMEOUT",
  "UND_ERR_HEADERS_TIMEOUT",
  "UND_ERR_BODY_TIMEOUT",
  "UND_ERR_CLOSED",
]);

/**
 * True when an RPC failed because nothing answered: no daemon on the
 * socket, or the remote listener unreachable (UNAVAILABLE, a Node errno
 * anywhere in the cause chain, or fetch's own failure, a TypeError, which
 * is what a dropped network looks like over gRPC-Web). The call may have
 * reached the daemon, so it is retried with the same request id.
 */
export function isConnectionFailure(err: unknown): boolean {
  if (err instanceof ConnectError) {
    if (err.code === Code.Unavailable) return true;
    if (err.code === Code.Unknown && err.cause instanceof TypeError) return true;
  }
  for (let e: unknown = err, depth = 0; e != null && depth < 8; depth++) {
    if (typeof e === "object" && "code" in e && CONNECTION_ERRNOS.has(String(e.code))) return true;
    e = typeof e === "object" && "cause" in e ? e.cause : undefined;
  }
  return false;
}

export const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

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
