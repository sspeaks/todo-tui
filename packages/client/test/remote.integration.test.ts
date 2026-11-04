// The remote listener (ADR-0004) from the portable client: gRPC-Web with
// the bearer token, over Node's fetch, against the real daemon. The same
// conformance suites as the Unix socket run over it. Runs only with
// TODO_BIN pointing at a built `todo` binary.
import { execFile } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { Code, ConnectError } from "@connectrpc/connect";
import { afterEach, describe, expect, it } from "vitest";
import { type TodoConnection, connect } from "../src/connect.js";
import { ClientKind } from "../src/gen/todo/v1/system_service_pb.js";
import { CreatedVia } from "../src/gen/todo/v1/item_pb.js";
import type { LiveStore } from "../src/live-store.js";
import { captureItem } from "../src/mutations.js";
import { REMOTE_API_MINOR, type RemoteConnection, connectRemote, createRemoteLiveStore, isUnreachable, parsePairingLink } from "../src/remote.js";
import { CAPTURE_ZONE, captureConformance } from "./conformance/capture.js";
import { FILTER_ZONE, filterViewsConformance } from "./conformance/filter-views.js";
import { RECURRENCE_ZONE, recurrenceConformance } from "./conformance/recurrence.js";
import { undoRelationsConformance } from "./conformance/undo-relations.js";

const TODO_BIN = process.env.TODO_BIN;
const run = promisify(execFile);

const cleanups: (() => Promise<void> | void)[] = [];
afterEach(async () => {
  for (const c of cleanups.splice(0).reverse()) await c();
});

async function waitFor(cond: () => boolean, timeoutMs = 5000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!cond()) {
    if (Date.now() > deadline) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 10));
  }
}

/** A loopback port nothing listens on, for the daemon to bind. */
function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const s = createServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const port = (s.address() as { port: number }).port;
      s.close(() => resolve(port));
    });
  });
}

/**
 * A fresh TODO_TUI_HOME whose config turns the remote listener on, a
 * daemon spawned over the socket, and a device paired with `todo remote
 * pair`; shut down after the test, or by `close` when not `tracked`.
 */
async function home(extra: NodeJS.ProcessEnv = {}, tracked = true) {
  const dir = mkdtempSync(join(tmpdir(), "todo-remote-"));
  const port = await freePort();
  writeFileSync(join(dir, "config.toml"), `[remote]\nlisten = "127.0.0.1:${port}"\n`);
  const env = { ...process.env, ...extra, TODO_TUI_HOME: dir };
  const baseUrl = `http://127.0.0.1:${port}`;
  const local: TodoConnection = await connect({ env, clientKind: ClientKind.ONESHOT, todoBin: TODO_BIN!, spawnBudgetMs: 5000 });
  const pair = async (name: string) => {
    const { stdout } = await run(TODO_BIN!, ["remote", "pair", "--name", name, "--url", baseUrl, "--no-qr"], { env });
    const link = /^Link: +(\S+)$/m.exec(stdout)?.[1];
    const pairing = link === undefined ? undefined : parsePairingLink(link);
    if (!pairing) throw new Error(`no pairing link in:\n${stdout}`);
    expect(pairing.url).toBe(baseUrl);
    return pairing.token;
  };
  const token = await pair("phone");
  const remote = (t = token) => connectRemote({ baseUrl, token: t });
  const close = async () => {
    await local.system.shutdown({}).catch(() => {});
    local.close();
    rmSync(dir, { recursive: true, force: true });
  };
  if (tracked) cleanups.push(close);
  return { dir, env, baseUrl, token, local, pair, remote, close, add: (text: string) => run(TODO_BIN!, ["add", text], { env }) };
}

function track(store: LiveStore) {
  cleanups.push(() => store.stop());
  return store;
}

const code = (p: Promise<unknown>) => p.then(() => undefined, (e: unknown) => ConnectError.from(e).code);

describe.skipIf(!TODO_BIN)("remote listener against $TODO_BIN", { timeout: 60_000 }, () => {
  it("says hello with a paired token, and refuses a bad, missing or revoked one", async () => {
    const h = await home();
    const conn = await h.remote();
    expect(conn.serverInfo.apiMinor).toBeGreaterThanOrEqual(REMOTE_API_MINOR);
    expect(await code(h.remote("todo_nope"))).toBe(Code.Unauthenticated);
    expect(await code(h.remote(""))).toBe(Code.Unauthenticated);
    const other = await h.pair("tablet");
    await h.remote(other);
    await run(TODO_BIN!, ["remote", "revoke", "tablet"], { env: h.env });
    expect(await code(h.remote(other))).toBe(Code.Unauthenticated);
    // The phone's token still works.
    await conn.system.getServerInfo({ clientKind: ClientKind.MOBILE });
  });

  it("cannot stop the daemon", async () => {
    const h = await home();
    const conn = await h.remote();
    expect(await code(conn.system.shutdown({}))).toBe(Code.PermissionDenied);
    expect(await code(conn.system.shutdown({ restart: true }))).toBe(Code.PermissionDenied);
    await h.local.system.getServerInfo({});
  });

  it("captures as MOBILE, and a LiveStore follows the laptop's changes over the stream", async () => {
    const h = await home();
    const conn: RemoteConnection = await h.remote();
    const store = track(createRemoteLiveStore(conn, { debounceMs: 10 }));
    const view = store.view({});
    const titles = () => view.getSnapshot().items.map((i) => i.title);
    const unsubscribe = view.subscribe(() => {});
    cleanups.push(unsubscribe);
    await waitFor(() => store.getConnection().state === "live");

    const { item } = await captureItem(conn.items, "call dentist ^fri", { createdVia: CreatedVia.MOBILE });
    expect(item?.createdVia).toBe(CreatedVia.MOBILE);
    await waitFor(() => titles().includes("call dentist"));

    await h.add("from the laptop");
    await waitFor(() => titles().includes("from the laptop"));
  });

  it("is unreachable, not unauthenticated, when nothing listens", async () => {
    const port = await freePort();
    const err = await connectRemote({ baseUrl: `http://127.0.0.1:${port}`, token: "todo_x" }).then(
      () => undefined,
      (e: unknown) => e,
    );
    expect(isUnreachable(err)).toBe(true);
    // A refused token is an answer, not an outage.
    const h = await home();
    expect(isUnreachable(await h.remote("todo_nope").catch((e: unknown) => e))).toBe(false);
  });
});

describe.skipIf(!TODO_BIN)("undo, parent rules and relations over the remote listener", { timeout: 60_000 }, () => {
  undoRelationsConformance(async () => {
    const conn = await (await home()).remote();
    return { items: conn.items, transport: conn.transport };
  });
});

describe.skipIf(!TODO_BIN)("filters, views, counts and ParseDate over the remote listener", { timeout: 60_000 }, () => {
  filterViewsConformance(async () => {
    const h = await home({ TZ: FILTER_ZONE }, false);
    const conn = await h.remote();
    return { items: conn.items, transport: conn.transport, timeZone: FILTER_ZONE, close: h.close };
  });
});

describe.skipIf(!TODO_BIN)("capture syntax and waiting over the remote listener", { timeout: 60_000 }, () => {
  captureConformance(async () => {
    const h = await home({ TZ: CAPTURE_ZONE }, false);
    const conn = await h.remote();
    return { items: conn.items, timeZone: CAPTURE_ZONE, close: h.close };
  });
});

describe.skipIf(!TODO_BIN)("recurrence over the remote listener", { timeout: 60_000 }, () => {
  recurrenceConformance(async () => {
    const conn = await (await home({ TZ: RECURRENCE_ZONE })).remote();
    return { items: conn.items, timeZone: RECURRENCE_ZONE };
  });
});
