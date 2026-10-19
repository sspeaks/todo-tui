# ADR-0002: Connect-ES over the daemon's Unix socket

- Status: accepted
- Date: 2026-10-18
- Settles: design 9.2 open question #1

## Context

The TS clients (TUI, MCP server) talk to the tonic daemon over a Unix domain socket.
The design's default was Connect-ES (`@connectrpc/connect-node`, gRPC protocol) over
HTTP/2 on that socket, with `@grpc/grpc-js` + `ts-proto` as the fallback should
Connect-ES not cope with UDS, tonic, or long-lived server streams.

## Decision

Keep **Connect-ES**. `packages/client/src/transport.ts` builds a `createGrpcTransport`
whose `Http2SessionManager` dials with `createConnection: () => net.connect(socket)`
against `http://localhost` (the `:authority` from design 4.2), with HTTP/2 keepalive
PINGs (15s interval, 5s timeout) to detect a dead daemon on idle streams. Codegen stays
`protoc-gen-es` v2 into `packages/client/src/gen/`. The grpc-js fallback is not needed.

## Evidence

Run against a real `target/debug/todo` daemon (tonic 0.14, hyper HTTP/2 over UDS),
`TODO_BIN=target/debug/todo vitest run test/uds.integration.test.ts`: 7 passed, 1 skipped.

| Concern | Test | Result |
|---|---|---|
| Unary calls, auto-spawn | GetServerInfo/CaptureItem/ListItems after `connect()` spawns the daemon | pass |
| Server streams | `WatchService.Subscribe` receives a Change made by `todo add` | pass |
| Abort | aborting a stream leaves the HTTP/2 session usable for later calls | pass |
| Error details | FAILED_PRECONDITION `INTERACTIVE_CLIENTS_CONNECTED` and INVALID_ARGUMENT `page_token` decoded via `findDetails` (ErrorInfo, BadRequest) | pass |
| Large responses | 5k items via paged ListItems | pass |
| Reconnect | daemon `Shutdown`, LiveStore respawns it and resumes from its revision | pass |
| Concurrency | 10 concurrent Subscribe streams on one connection | pass |
| ABORTED / REVISION_MISMATCH | no P0 RPC can trigger it | skipped (P1) |

The same transport also passes the unit suite against a Node HTTP/2 gRPC server on a
Unix socket, and `scripts/smoke.sh` exercises it end to end (a TS watcher sees a
`todo add` over Subscribe, including through the long-path `/tmp` socket fallback).
The Ink TUI was run in a pty against a live daemon and rendered an item added with
`todo add` while it was running.

## Consequences

- One transport for all TS clients; `@todo-tui/client/core` is the React-free entry
  point for `mcp`.
- `ConnectError` codes and details map one-to-one onto tonic `Status` + `tonic-types`
  details, so error handling needs no translation layer.
- Should a future Node or Connect release regress, the fallback remains confined to
  `transport.ts` and `gen/`.
