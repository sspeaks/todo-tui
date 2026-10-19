// The Connect-over-UDS transport against an in-process Node HTTP/2 gRPC server.
// The same cases run against the Rust daemon in uds.integration.test.ts.
import { chmodSync, mkdtempSync, rmSync, symlinkSync } from "node:fs";
import * as http2 from "node:http2";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, createClient } from "@connectrpc/connect";
import { connectNodeAdapter } from "@connectrpc/connect-node";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { ErrorInfoSchema } from "../src/gen/google/rpc/error_details_pb.js";
import { ItemSchema } from "../src/gen/todo/v1/item_pb.js";
import { ItemService } from "../src/gen/todo/v1/item_service_pb.js";
import { SystemService } from "../src/gen/todo/v1/system_service_pb.js";
import { WatchService } from "../src/gen/todo/v1/watch_service_pb.js";
import { connect } from "../src/connect.js";
import { isConnectionFailure } from "../src/spawn.js";
import { UnsafeRuntimeDirError, checkRuntimeDir, createTodoTransport, guardedDirs } from "../src/transport.js";

let dir: string;
let socket: string;
let server: http2.Http2Server;

beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "tt-"));
  socket = join(dir, "daemon.sock");
  server = http2.createServer(
    connectNodeAdapter({
      routes: ({ service }) => {
        service(SystemService, {
          getServerInfo: (req) => {
            if (req.requiredApiMinor > 5) {
              throw new ConnectError("too new", Code.FailedPrecondition, undefined, [
                { desc: ErrorInfoSchema, value: { reason: "API_MINOR", domain: "todo.v1", metadata: { have: "5" } } },
              ]);
            }
            return { daemonVersion: "node-test", apiMinor: 5 };
          },
          shutdown: () => ({}),
        });
        service(ItemService, {
          captureItem: () => ({}),
          listItems: (req) => ({
            items: Array.from({ length: req.pageSize }, (_, i) =>
              create(ItemSchema, { id: String(i), title: `item ${i} ${"x".repeat(200)}` }),
            ),
            revision: 7n,
          }),
        });
        service(WatchService, {
          async *subscribe(req, ctx) {
            for (let r = (req.sinceRevision ?? 0n) + 1n; !ctx.signal.aborted; r++) {
              yield { message: { case: "change", value: { revision: r, id: String(r) } } };
              await new Promise((res) => setTimeout(res, 5));
            }
          },
        });
      },
    }),
  );
  await new Promise<void>((res) => server.listen(socket, res));
});

afterAll(async () => {
  await new Promise((res) => server.close(res));
  rmSync(dir, { recursive: true, force: true });
});

describe("createTodoTransport over a Unix socket", () => {
  it("makes unary calls", async () => {
    const t = createTodoTransport(socket);
    const info = await createClient(SystemService, t).getServerInfo({});
    expect(info.daemonVersion).toBe("node-test");
    t.close();
  });

  it("decodes error details", async () => {
    const t = createTodoTransport(socket);
    const err = await createClient(SystemService, t)
      .getServerInfo({ requiredApiMinor: 9 })
      .catch((e: unknown) => ConnectError.from(e));
    expect(err).toBeInstanceOf(ConnectError);
    expect((err as ConnectError).code).toBe(Code.FailedPrecondition);
    const [info] = (err as ConnectError).findDetails(ErrorInfoSchema);
    expect(info?.reason).toBe("API_MINOR");
    expect(info?.metadata.have).toBe("5");
    t.close();
  });

  it("streams, aborts, and runs concurrent streams", async () => {
    const t = createTodoTransport(socket);
    const watch = createClient(WatchService, t);
    const take = async (since: bigint, n: number) => {
      const ac = new AbortController();
      const got: bigint[] = [];
      try {
        for await (const m of watch.subscribe({ sinceRevision: since }, { signal: ac.signal })) {
          if (m.message.case === "change") got.push(m.message.value.revision);
          if (got.length === n) ac.abort();
        }
      } catch (e) {
        expect(ConnectError.from(e).code).toBe(Code.Canceled);
      }
      return got;
    };
    const results = await Promise.all(Array.from({ length: 10 }, (_, i) => take(BigInt(i * 100), 5)));
    results.forEach((got, i) => expect(got[0]).toBe(BigInt(i * 100 + 1)));
    expect(results.every((r) => r.length === 5)).toBe(true);
    t.close();
  });

  it("carries large responses", async () => {
    const t = createTodoTransport(socket);
    const res = await createClient(ItemService, t).listItems({ pageSize: 5000 });
    expect(res.items).toHaveLength(5000);
    t.close();
  });

  it("reports a missing socket as a connection failure", async () => {
    const t = createTodoTransport(join(dir, "nope.sock"));
    const err = await createClient(SystemService, t)
      .getServerInfo({})
      .catch((e: unknown) => e);
    expect(isConnectionFailure(err)).toBe(true);
    t.close();
    const t2 = createTodoTransport(join(dir, "no-such-dir", "daemon.sock"));
    const err2 = await createClient(SystemService, t2)
      .getServerInfo({})
      .catch((e: unknown) => e);
    expect(isConnectionFailure(err2)).toBe(true);
    t2.close();
  });

  it("connect() handshakes without spawning when the daemon answers", async () => {
    const conn = await connect({ socketPath: socket, autoSpawn: false });
    expect(conn.serverInfo.apiMinor).toBe(5);
    conn.close();
  });

  it("connect() fails fast when autoSpawn is off and nothing listens", async () => {
    await expect(connect({ socketPath: join(dir, "nope.sock"), autoSpawn: false })).rejects.toSatisfy(isConnectionFailure);
  });

  it("connect() spawns $TODO_BIN when nothing listens", async () => {
    await expect(
      connect({ socketPath: join(dir, "nope.sock"), todoBin: "/nonexistent/todo", spawnBudgetMs: 50 }),
    ).rejects.toThrow(/cannot run \/nonexistent\/todo/);
  });

  it("refuses a socket whose directory others can reach, even if it answers", async () => {
    chmodSync(dir, 0o755);
    try {
      expect(() => checkRuntimeDir(dir)).toThrow(UnsafeRuntimeDirError);
      await expect(connect({ socketPath: socket, autoSpawn: false })).rejects.toThrow(/accessible by other users/);
      const t = createTodoTransport(socket);
      const err = await createClient(SystemService, t)
        .getServerInfo({})
        .catch((e: unknown) => e);
      expect(String(ConnectError.from(err).message)).toMatch(/accessible by other users/);
      expect(isConnectionFailure(err)).toBe(false);
      t.close();
    } finally {
      chmodSync(dir, 0o700);
    }
  });

  it("refuses a symlinked runtime dir and treats a missing one as not running", () => {
    const link = join(dir, "link");
    symlinkSync(dir, link);
    expect(() => checkRuntimeDir(link)).toThrow(/not a directory/);
    expect(() => checkRuntimeDir(join(dir, "missing"))).toThrow(expect.objectContaining({ code: "ENOENT" }));
    checkRuntimeDir(dir);
  });

  it("guards the /tmp fallback parent too", () => {
    expect(guardedDirs("/tmp/todo-tui-7/h-0123abcd", 7)).toEqual(["/tmp/todo-tui-7/h-0123abcd", "/tmp/todo-tui-7"]);
    expect(guardedDirs("/tmp/todo-tui-7", 7)).toEqual(["/tmp/todo-tui-7"]);
    expect(guardedDirs("/x/run", 7)).toEqual(["/x/run"]);
  });
});
