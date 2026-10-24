// The upgrade restart (design 4.2): upgradeRestart's steps directly, the
// helpers that wait for the old daemon, and connect() against an in-process
// Node HTTP/2 server standing in for an old daemon.
import { type ChildProcess, spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import * as http2 from "node:http2";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import { connectNodeAdapter } from "@connectrpc/connect-node";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ErrorInfoSchema } from "../src/gen/google/rpc/error_details_pb.js";
import { GetServerInfoResponseSchema, SystemService } from "../src/gen/todo/v1/system_service_pb.js";
import {
  CLIENT_API_MINOR,
  DaemonRestartRefusedError,
  DaemonTooOldError,
  type UpgradeRestartSteps,
  connect,
  upgradeRestart,
} from "../src/connect.js";
import { envFromProcess, resolvePaths } from "../src/paths.js";
import { readDaemonPid, waitForDaemonExit } from "../src/spawn.js";

const info = (apiMinor: number) => create(GetServerInfoResponseSchema, { daemonVersion: `0.${apiMinor}.0`, apiMinor });

const refused = () =>
  new ConnectError("restart required; close the other 1 interactive session(s)", Code.FailedPrecondition, undefined, [
    {
      desc: ErrorInfoSchema,
      value: { reason: "INTERACTIVE_CLIENTS_CONNECTED", domain: "todo-tui", metadata: { interactive_clients: "1" } },
    },
  ]);

function fakeSteps(opts: { shutdownError?: Error; apiMinorAfter?: number } = {}) {
  const log: string[] = [];
  const steps: UpgradeRestartSteps = {
    shutdown: async () => {
      log.push("shutdown");
      if (opts.shutdownError) throw opts.shutdownError;
    },
    waitForExit: async () => void log.push("wait"),
    spawn: async () => void log.push("spawn"),
    handshake: async () => {
      log.push("handshake");
      return info(opts.apiMinorAfter ?? 1);
    },
  };
  return { steps, log };
}

describe("upgradeRestart", () => {
  it("shuts down, waits, respawns and handshakes again", async () => {
    const { steps, log } = fakeSteps();
    expect((await upgradeRestart(info(0), 1, steps)).apiMinor).toBe(1);
    expect(log).toEqual(["shutdown", "wait", "spawn", "handshake"]);
  });

  it("reports a refused restart as DaemonTooOldError that asks to close other sessions", async () => {
    const { steps, log } = fakeSteps({ shutdownError: refused() });
    const err = await upgradeRestart(info(0), 1, steps).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(DaemonRestartRefusedError);
    expect(err).toBeInstanceOf(DaemonTooOldError);
    expect((err as Error).message).toMatch(/^restart required .*api_minor 0.*; close other todo sessions first$/);
    expect((err as DaemonTooOldError).serverInfo.apiMinor).toBe(0);
    expect(log).toEqual(["shutdown"]);
  });

  it("throws DaemonTooOldError when the respawned daemon is still too old", async () => {
    const { steps, log } = fakeSteps({ apiMinorAfter: 0 });
    const err = await upgradeRestart(info(0), 1, steps).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(DaemonTooOldError);
    expect(err).not.toBeInstanceOf(DaemonRestartRefusedError);
    expect(log).toEqual(["shutdown", "wait", "spawn", "handshake"]);
  });

  it("carries on when the daemon is already gone", async () => {
    const { steps, log } = fakeSteps({ shutdownError: new ConnectError("connection reset", Code.Unavailable) });
    expect((await upgradeRestart(info(0), 1, steps)).apiMinor).toBe(1);
    expect(log).toEqual(["shutdown", "wait", "spawn", "handshake"]);
  });

  it("passes on any other shutdown failure", async () => {
    for (const e of [new ConnectError("no", Code.PermissionDenied), new ConnectError("other", Code.FailedPrecondition)]) {
      const { steps, log } = fakeSteps({ shutdownError: e });
      await expect(upgradeRestart(info(0), 1, steps)).rejects.toBe(e);
      expect(log).toEqual(["shutdown"]);
    }
  });
});

describe("waiting for the old daemon", () => {
  let dir: string;
  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), "tt-wait-"));
  });
  afterEach(() => rmSync(dir, { recursive: true, force: true }));

  it("reads the pid from the lock file", () => {
    const lock = join(dir, "daemon.lock");
    expect(readDaemonPid(lock)).toBeUndefined();
    for (const [text, pid] of [
      ["4242\n", 4242],
      ["", undefined],
      ["0\n", undefined],
      ["12ab", undefined],
      ["-5", undefined],
    ] as const) {
      writeFileSync(lock, text);
      expect(readDaemonPid(lock)).toBe(pid);
    }
  });

  it("follows the process when the pid is known", async () => {
    const child = spawn("sleep", ["0.3"]);
    const start = Date.now();
    await waitForDaemonExit(child.pid, join(dir, "unused.sock"));
    expect(Date.now() - start).toBeGreaterThanOrEqual(250);
    expect(child.exitCode).toBe(0);
  });

  it("gives up after the timeout", async () => {
    const child = spawn("sleep", ["10"]);
    try {
      await expect(waitForDaemonExit(child.pid, join(dir, "unused.sock"), 100)).rejects.toThrow(
        `daemon (pid ${child.pid}) still running after 100ms`,
      );
    } finally {
      child.kill();
    }
  });

  it("without a pid, waits until the socket stops accepting connections", async () => {
    const sock = join(dir, "daemon.sock");
    const server = createServer((s) => s.destroy());
    await new Promise<void>((r) => server.listen(sock, r));
    setTimeout(() => server.close(), 150);
    const start = Date.now();
    await waitForDaemonExit(undefined, sock);
    expect(Date.now() - start).toBeGreaterThanOrEqual(100);
    await waitForDaemonExit(undefined, sock);
  });
});

describe("connect() against an old daemon", () => {
  let home: string;
  let env: NodeJS.ProcessEnv;
  let todoBin: string;
  /** The arguments of each run of todoBin, one line per run. */
  const spawned = () => (existsSync(join(home, "spawned")) ? readFileSync(join(home, "spawned"), "utf8") : "");
  let server: http2.Http2Server;
  /** Stands in for the old daemon's process, whose pid is in the lock. */
  let old: ChildProcess;
  let daemon: { apiMinor: number; apiMinorAfterRestart: number; refuse: boolean; shutdowns: boolean[] };

  beforeEach(async () => {
    home = mkdtempSync(join(tmpdir(), "tt-up-"));
    env = { ...process.env, TODO_TUI_HOME: home };
    const paths = resolvePaths(envFromProcess(env));
    mkdirSync(paths.runtime, { recursive: true, mode: 0o700 });
    mkdirSync(paths.data, { recursive: true });
    old = spawn("sleep", ["30"]);
    writeFileSync(paths.lock, `${old.pid}\n`);
    // "Spawning" records its arguments and succeeds at once; the server below
    // plays the new daemon too.
    todoBin = join(home, "todo");
    writeFileSync(todoBin, `#!/bin/sh\necho "$@" >> '${join(home, "spawned")}'\n`, { mode: 0o755 });
    daemon = { apiMinor: 0, apiMinorAfterRestart: CLIENT_API_MINOR, refuse: false, shutdowns: [] };
    server = http2.createServer(
      connectNodeAdapter({
        routes: ({ service }) => {
          service(SystemService, {
            getServerInfo: () => ({ daemonVersion: "node-test", apiMinor: daemon.apiMinor }),
            shutdown: (req) => {
              daemon.shutdowns.push(req.restart);
              if (daemon.refuse) throw refused();
              daemon.apiMinor = daemon.apiMinorAfterRestart;
              setTimeout(() => old.kill(), 200);
              return {};
            },
          });
        },
      }),
    );
    await new Promise<void>((r) => server.listen(paths.socket, r));
  });

  afterEach(async () => {
    old.kill();
    await new Promise((r) => server.close(r));
    rmSync(home, { recursive: true, force: true });
  });

  it("restarts it, waits for the old process to exit and handshakes with the new one", async () => {
    const start = Date.now();
    const conn = await connect({ env, todoBin });
    conn.close();
    expect(conn.serverInfo.apiMinor).toBe(CLIENT_API_MINOR);
    expect(daemon.shutdowns).toEqual([true]);
    expect(Date.now() - start).toBeGreaterThanOrEqual(150);
    expect(old.signalCode).toBe("SIGTERM");
    expect(spawned()).toBe("daemon --detach\n");
  });

  it("asks to close other sessions when the daemon refuses", async () => {
    daemon.refuse = true;
    await expect(connect({ env, todoBin })).rejects.toThrow(DaemonRestartRefusedError);
    expect(daemon.shutdowns).toEqual([true]);
    expect(old.exitCode === null && old.signalCode === null).toBe(true);
    expect(spawned()).toBe("");
  });

  it("fails with DaemonTooOldError when the restarted daemon is still too old", async () => {
    daemon.apiMinorAfterRestart = 0;
    const err = await connect({ env, todoBin }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(DaemonTooOldError);
    expect(err).not.toBeInstanceOf(DaemonRestartRefusedError);
    expect(daemon.shutdowns).toEqual([true]);
    expect(spawned()).toBe("daemon --detach\n");
  });

  it("does not restart without auto-spawn", async () => {
    await expect(connect({ env, todoBin, autoSpawn: false })).rejects.toThrow(DaemonTooOldError);
    expect(daemon.shutdowns).toEqual([]);
    expect(spawned()).toBe("");
  });
});
