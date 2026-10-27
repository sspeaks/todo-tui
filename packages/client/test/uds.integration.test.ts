// UDS spike (S0.7, ADR-0002): Connect-ES against the real tonic daemon.
// Runs only with TODO_BIN pointing at a built `todo` binary.
import { execFile } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { Code, ConnectError } from "@connectrpc/connect";
import { afterEach, describe, expect, it } from "vitest";
import { BadRequestSchema, ErrorInfoSchema } from "../src/gen/google/rpc/error_details_pb.js";
import { CreatedVia, type Item } from "../src/gen/todo/v1/item_pb.js";
import { ClientKind } from "../src/gen/todo/v1/system_service_pb.js";
import { type TodoConnection, connect, createLiveStore } from "../src/connect.js";
import { MAX_CAPTURE_BYTES } from "../src/limits.js";
import type { LiveStore } from "../src/live-store.js";
import { updateItem } from "../src/mutations.js";
import { resolvePaths, envFromProcess } from "../src/paths.js";
import { ulid } from "../src/ulid.js";
import { CAPTURE_ZONE, captureConformance } from "./conformance/capture.js";
import { FILTER_ZONE, filterViewsConformance } from "./conformance/filter-views.js";
import { undoRelationsConformance } from "./conformance/undo-relations.js";

const TODO_BIN = process.env.TODO_BIN;
const run = promisify(execFile);

async function waitFor(cond: () => boolean, timeoutMs = 5000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!cond()) {
    if (Date.now() > deadline) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 10));
  }
}

const cleanups: (() => Promise<void> | void)[] = [];
afterEach(async () => {
  for (const c of cleanups.splice(0).reverse()) await c();
});

/**
 * A fresh TODO_TUI_HOME with an auto-spawned daemon, which sees `extra` in
 * its environment; shut down after the test, or by `close` when not
 * `tracked`.
 */
async function home(extra: NodeJS.ProcessEnv = {}, tracked = true) {
  const dir = mkdtempSync(join(tmpdir(), "todo-uds-"));
  const env = { ...process.env, ...extra, TODO_TUI_HOME: dir };
  const conns: TodoConnection[] = [];
  const open = async (kind = ClientKind.INTERACTIVE) => {
    const c = await connect({ env, clientKind: kind, todoBin: TODO_BIN!, spawnBudgetMs: 5000 });
    conns.push(c);
    return c;
  };
  const add = (text: string) => run(TODO_BIN!, ["add", text], { env });
  const close = async () => {
    for (const c of conns) c.close();
    try {
      // Any api_minor will do for a shutdown.
      const c = await connect({ env, autoSpawn: false, clientKind: ClientKind.ONESHOT, requiredApiMinor: 0 });
      await c.system.shutdown({}).catch(() => {});
      c.close();
    } catch {
      // already down
    }
    rmSync(dir, { recursive: true, force: true });
  };
  if (tracked) cleanups.push(close);
  return { dir, env, open, add, close, socket: resolvePaths(envFromProcess(env)).socket };
}

function track(store: LiveStore) {
  cleanups.push(() => store.stop());
  return store;
}

async function listAll(conn: TodoConnection): Promise<Item[]> {
  const out: Item[] = [];
  let pageToken = "";
  do {
    const res = await conn.items.listItems({ pageToken });
    out.push(...res.items);
    pageToken = res.nextPageToken;
  } while (pageToken);
  return out;
}

describe.skipIf(!TODO_BIN)("UDS integration against $TODO_BIN", { timeout: 60_000 }, () => {
  it("auto-spawns and makes unary calls", async () => {
    const h = await home();
    const conn = await h.open();
    expect(conn.serverInfo.daemonVersion).not.toBe("");
    const res = await conn.items.captureItem({ createdVia: CreatedVia.TUI, requestId: ulid(), text: "hello" });
    expect(res.item?.title).toBe("hello");
    expect(res.result?.revision).toBeGreaterThan(0n);
    const list = await conn.items.listItems({});
    expect(list.items.map((i) => i.title)).toContain("hello");
    expect(list.revision).toBe(res.result?.revision);
  });

  it("streams a change made by `todo add`", async () => {
    const h = await home();
    const conn = await h.open();
    const { revision } = await conn.items.listItems({});
    const ac = new AbortController();
    const stream = conn.watch.subscribe({ sinceRevision: revision }, { signal: ac.signal });
    const seen: string[] = [];
    let caughtUp = false;
    const reader = (async () => {
      for await (const m of stream) {
        if (m.message.case === "caughtUp") {
          caughtUp = true;
          void h.add("from cli");
        }
        if (m.message.case === "change" && m.message.value.resource.case === "item") {
          seen.push(m.message.value.resource.value.title);
          ac.abort();
        }
      }
    })().catch((e: unknown) => {
      if (ConnectError.from(e).code !== Code.Canceled) throw e;
    });
    await reader;
    expect(caughtUp).toBe(true);
    expect(seen).toEqual(["from cli"]);
  });

  it("aborts a stream and keeps the connection usable", async () => {
    const h = await home();
    const conn = await h.open();
    const ac = new AbortController();
    const it = conn.watch.subscribe({}, { signal: ac.signal })[Symbol.asyncIterator]();
    expect((await it.next()).value?.message.case).toBe("caughtUp");
    ac.abort();
    await expect(it.next()).rejects.toSatisfy((e: unknown) => ConnectError.from(e).code === Code.Canceled);
    expect((await conn.system.getServerInfo({})).apiMinor).toBeGreaterThanOrEqual(0);
  });

  it("decodes ErrorInfo and BadRequest details", async () => {
    const h = await home();
    await h.open();
    const other = await h.open();
    const err = ConnectError.from(await other.system.shutdown({ restart: true }).catch((e: unknown) => e));
    expect(err.code).toBe(Code.FailedPrecondition);
    expect(err.findDetails(ErrorInfoSchema)[0]?.reason).toBe("INTERACTIVE_CLIENTS_CONNECTED");

    const bad = ConnectError.from(await other.items.listItems({ pageToken: "garbage" }).catch((e: unknown) => e));
    expect(bad.code).toBe(Code.InvalidArgument);
    expect(bad.findDetails(BadRequestSchema)[0]?.fieldViolations[0]?.field).toBe("page_token");
  });

  it("agrees with the daemon on MAX_CAPTURE_BYTES", async () => {
    const h = await home();
    const conn = await h.open();
    const capture = (text: string) => conn.items.captureItem({ requestId: ulid(), text, createdVia: CreatedVia.TUI });
    const longest = "é".repeat(MAX_CAPTURE_BYTES / 2);
    expect((await capture(longest)).item?.title).toBe(longest);
    const err = ConnectError.from(await capture(`${longest}x`).catch((e: unknown) => e));
    expect(err.code).toBe(Code.InvalidArgument);
    expect(err.findDetails(BadRequestSchema)[0]?.fieldViolations[0]).toMatchObject({ field: "text", description: "is longer than 64 KiB" });
  });

  it("decodes ABORTED / REVISION_MISMATCH", async () => {
    const h = await home();
    const conn = await h.open();
    const { item } = await conn.items.captureItem({ createdVia: CreatedVia.TUI, requestId: ulid(), text: "edit me" });
    const { id, revision } = item!;

    const rename = (expectedRevision: bigint) =>
      updateItem(conn.items, { id, title: "renamed" }, ["title"], { expectedRevision });
    const err = ConnectError.from(await rename(revision + 1n).catch((e: unknown) => e));
    expect(err.code).toBe(Code.Aborted);
    expect(err.findDetails(ErrorInfoSchema)[0]?.reason).toBe("REVISION_MISMATCH");
    expect((await conn.items.getItem({ id })).item?.title).toBe("edit me");

    const ok = await rename(revision);
    expect(ok.item?.title).toBe("renamed");
    expect(ok.item?.revision).toBeGreaterThan(revision);
  });

  it("lists 5k items", async () => {
    const h = await home();
    const conn = await h.open();
    const N = 5000;
    for (let i = 0; i < N; i += 100) {
      await Promise.all(
        Array.from({ length: 100 }, (_, j) =>
          conn.items.captureItem({ createdVia: CreatedVia.TUI, requestId: ulid(), text: `item ${i + j}` }),
        ),
      );
    }
    const items = await listAll(conn);
    expect(items).toHaveLength(N);
    expect(new Set(items.map((i) => i.id)).size).toBe(N);
  });

  it("resumes the live store across a daemon restart", async () => {
    const h = await home();
    const conn = await h.open();
    const store = track(createLiveStore(conn, { backoff: { initialMs: 50, maxMs: 500 } }));
    const view = store.view();
    await waitFor(() => store.getConnection().state === "live");
    await h.add("before");
    await waitFor(() => view.getSnapshot().items.some((i) => i.title === "before"));
    const rev = store.getConnection().revision;

    const killer = await h.open(ClientKind.ONESHOT);
    await killer.system.shutdown({});
    await waitFor(() => store.getConnection().state === "reconnecting");
    // The store's ensureDaemon respawns it; `todo add` would too.
    await waitFor(() => store.getConnection().state === "live", 10_000);
    // The new daemon still counts the store's connection as interactive (4.2).
    const other = await h.open(ClientKind.ONESHOT);
    const refused = ConnectError.from(await other.system.shutdown({ restart: true }).catch((e: unknown) => e));
    expect(refused.code).toBe(Code.FailedPrecondition);
    expect(refused.findDetails(ErrorInfoSchema)[0]?.reason).toBe("INTERACTIVE_CLIENTS_CONNECTED");
    await h.add("after");
    await waitFor(() => view.getSnapshot().items.some((i) => i.title === "after"));
    expect(store.getConnection().revision).toBeGreaterThan(rev);
    expect(view.getSnapshot().items.map((i) => i.title)).toEqual(["before", "after"]);
  });

  it("serves 10 concurrent streams on one connection", async () => {
    const h = await home();
    const conn = await h.open();
    const { revision } = await conn.items.listItems({});
    const streams = Array.from({ length: 10 }, async () => {
      const ac = new AbortController();
      const titles: string[] = [];
      try {
        for await (const m of conn.watch.subscribe({ sinceRevision: revision }, { signal: ac.signal })) {
          if (m.message.case === "change" && m.message.value.resource.case === "item") {
            titles.push(m.message.value.resource.value.title);
            if (titles.length === 3) ac.abort();
          }
        }
      } catch (e) {
        if (ConnectError.from(e).code !== Code.Canceled) throw e;
      }
      return titles;
    });
    for (const t of ["a", "b", "c"]) await conn.items.captureItem({ createdVia: CreatedVia.TUI, requestId: ulid(), text: t });
    const results = await Promise.all(streams);
    for (const r of results) expect(r).toEqual(["a", "b", "c"]);
  });
});

describe.skipIf(!TODO_BIN)("undo, parent rules and relations against $TODO_BIN", { timeout: 60_000 }, () => {
  // The suite FakeDaemon runs too, each test on a fresh daemon. Every RPC is
  // the user's, so nothing can change an Item off the undo stack, and the
  // cases that need that (a partial undo, a dropped entry) are skipped here:
  // the store's tests cover them.
  undoRelationsConformance(async () => {
    const h = await home();
    const conn = await h.open();
    return { items: conn.items, transport: conn.transport };
  });
});

describe.skipIf(!TODO_BIN)("filters, views, counts and ParseDate against $TODO_BIN", { timeout: 60_000 }, () => {
  // The suite FakeDaemon runs too, on a daemon spawned in the same zone. A
  // daemon serves a whole describe of read-only cases, so it is closed by
  // the suite rather than after each test.
  filterViewsConformance(async () => {
    const h = await home({ TZ: FILTER_ZONE }, false);
    const conn = await h.open();
    return { items: conn.items, transport: conn.transport, timeZone: FILTER_ZONE, close: h.close };
  });
});

describe.skipIf(!TODO_BIN)("capture syntax and waiting against $TODO_BIN", { timeout: 60_000 }, () => {
  // The suite FakeDaemon runs too, on one daemon spawned in the same zone.
  captureConformance(async () => {
    const h = await home({ TZ: CAPTURE_ZONE }, false);
    const conn = await h.open();
    return { items: conn.items, timeZone: CAPTURE_ZONE, close: h.close };
  });
});
