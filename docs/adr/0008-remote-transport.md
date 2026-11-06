# ADR-0008: A remote listener for the phone app

- Status: accepted
- Date: 2026-11-03
- Amends: design 4.1 (shape), 4.2 (socket), 3.2 (`created_via`), 7.2 (schema)

## Context

The daemon serves `todo.v1` only on `daemon.sock`, mode `0600` (4.2): the socket's
file permissions are the whole access control, and nothing off the machine can
reach it. Design 4.1 anticipates "a phone app via a future network transport", and
4.2 says that transport is opt-in, carries TLS and a token, and must not change the
API.

The phone app is React Native (Expo). React Native's `fetch` cannot read HTTP/2
trailers, so native gRPC is out without a native module. The app reaches the laptop
over Tailscale.

## Decision

- **gRPC-Web on a loopback TCP listener.** When `config.toml` has
  `[remote] listen = "127.0.0.1:7743"`, the daemon serves the same four services on
  that address too, through `tonic-web` with HTTP/1.1 accepted. Without the section
  there is no listener, as before. A non-loopback address is refused unless
  `allow_non_loopback = true`.
- **TLS from `tailscale serve`, not the daemon.**
  `tailscale serve --bg --https=443 http://127.0.0.1:7743` gives the listener a
  valid certificate for `<machine>.<tailnet>.ts.net`, reachable only from the
  tailnet. The daemon gets no TLS code, no certificates and no renewal logic, and
  iOS needs no ATS exception.
- **A bearer token on every remote call.** `todo remote pair --name <n>` makes a
  random token, stores only its SHA-256 in `data/remote-tokens.json` (mode `0600`),
  and prints it once with a QR code for the app. The daemon reloads the file when it
  changes, so `todo remote revoke <id>` takes effect without a restart. A missing or
  unknown token is `UNAUTHENTICATED`. The Unix socket stays as it is, with no token.
- **No `Shutdown` from afar.** `SystemService.Shutdown` over the listener is
  `PERMISSION_DENIED`: the laptop's clients own the daemon's lifecycle.
- **The phone is its own client.** `ClientKind.MOBILE` (an interactive client, so
  an upgrade restart waits for it) and `CreatedVia.MOBILE` (a user capture). The
  CHECK on `items.created_via` cannot be altered in place, so a migration
  rebuilds `items` the way SQLite documents, with foreign keys off for that one
  migration and `foreign_key_check` run before it commits (`schema.rs`). On the
  phone-app branch it was 0005; merged after energy and sync, it is 0007, and a
  database that branch's daemon made is recognised and caught up.
- **Every client, not only the phone** (added at the merge). A laptop's `todo`,
  TUI and MCP server use a daemon elsewhere when its config says
  `[client] remote = "https://box.<tailnet>.ts.net"` with `credential =
  "keychain:…"` (or `env:`/`file:`, as Jira's), or when `TODO_REMOTE` and
  `TODO_REMOTE_TOKEN` are set. The CLI speaks gRPC-Web to the listener (tonic-web
  over rustls, the platform's roots); `todo` resolves the token once and hands
  the TUI and MCP bundles `TODO_REMOTE*`, and they connect through
  `connectAny`. Such a machine never spawns or restarts a daemon: a remote one
  too old says so. `todo daemon`, `restore`, `backup list` and `remote` act on
  this machine's own daemon or files, so they refuse. `todo add` sends what this
  machine's spool holds first, each with its spool id. `http://` is allowed only
  to loopback (a test, an SSH tunnel).
- **Hosting.** `nixosModules.default` (`nix/module.nix`) runs the daemon as a
  system service: its own user, `/var/lib/todo-tui` as its home, `settings` as
  its `config.toml`, secrets as systemd credentials (`credentials.jira =
  config.sops.secrets.jira-pat.path`, read as `credential = "systemd:jira"`:
  `LoadCredential=` copies a root-owned sops-nix or agenix secret for the
  service alone, so it needs no owner change) or an `environmentFile`, the listener on
  loopback and, with `tailscaleServe`, `tailscale serve` in front of it.
  `todo-tui-admin` runs the CLI as that user on that home (`remote pair`). A VM
  test (`checks.<linux>.module`) exercises it.
- **Scope.** The phone is a thin client of one daemon. When the laptop is asleep the
  app has no data. This is not multi-device sync, which stays a non-goal.

## Risks to retire early

1. gRPC-Web server streaming (`WatchService.Subscribe`) through `expo/fetch` and
   `tailscale serve`. A Node integration test over the listener comes first, then a
   device spike. If streaming fails, the fallback is to re-baseline the LiveStore on
   an interval while the app is in the foreground.
2. Metro resolving the workspace's raw-TS packages and their `.js` import
   extensions.

## Consequences

- `api_minor` 20 (9 on the phone-app branch).
- The tokens file is daemon state that the CLI writes, not configuration. The
  daemon still never writes `config.toml`.
- The client side is `@todo-tui/client/portable` (no Node APIs, checked by
  `test/portable.test.ts`) with `connectRemote`, `createRemoteLiveStore`,
  `parsePairingLink` and `isUnreachable` (`src/remote.ts`), over
  `@connectrpc/connect-web`. The conformance suites run over the listener too
  (`test/remote.integration.test.ts`).
- `ClientKind` is recorded per connection, and fetch spreads gRPC-Web calls
  over pooled HTTP/1.1 connections it chooses, so a phone's MOBILE kind marks
  only the connection its hello happened to use, for as long as fetch keeps
  it open. An upgrade restart may therefore not wait for a phone. The phone
  just reconnects.
- What the TUI and the app show alike (dates, row cells, repeat rules, the
  quick-add preview, the tabs' queries, `Latest`) is `@todo-tui/present`,
  typechecked without Node types.
- Anyone on the tailnet who holds a token has full read and write access to the
  todo list. Tokens are per device and revocable.
