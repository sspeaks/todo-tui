// The config (design 8.2) as a suite any SystemService runs:
// test/config-conformance.test.ts runs it against FakeDaemon,
// uds.integration.test.ts against the real daemon ($TODO_BIN), so the fake
// serves what the daemon does.
import { type Client, type Transport, createClient } from "@connectrpc/connect";
import { afterEach, describe, expect, it } from "vitest";
import { Topic } from "../../src/gen/todo/v1/common_pb.js";
import { Weekday } from "../../src/gen/todo/v1/item_pb.js";
import { SystemService } from "../../src/gen/todo/v1/system_service_pb.js";
import { WatchService } from "../../src/gen/todo/v1/watch_service_pb.js";
import { LiveStore } from "../../src/live-store.js";

/** A daemon with no config file, for one test. */
export interface ConfigTarget {
  transport: Transport;
  /**
   * Gives the daemon a config with archive_done_after_days = `days` and the
   * keymap override "K" = "move_up", as an edited file that was reloaded.
   */
  configure: (days: number) => Promise<void>;
  close?: () => void | Promise<void>;
}

async function waitFor(cond: () => boolean, timeoutMs = 5000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!cond()) {
    if (Date.now() > deadline) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 5));
  }
}

export function configConformance(make: () => Promise<ConfigTarget>): void {
  let target: ConfigTarget;
  let system: Client<typeof SystemService>;
  const stores: LiveStore[] = [];

  const setUp = async () => {
    target = await make();
    system = createClient(SystemService, target.transport);
  };

  afterEach(async () => {
    for (const s of stores.splice(0)) await s.stop();
    await target?.close?.();
  });

  describe("config", () => {
    it("GetConfig serves the defaults without a file", async () => {
      await setUp();
      const res = await system.getConfig({});
      const config = res.config!;
      expect(config.exists).toBe(false);
      expect(config.configVersion).toBe(1);
      expect(config.ui).toMatchObject({ reviewDay: Weekday.MONDAY, archiveDoneAfterDays: 7 });
      expect(config.backup).toMatchObject({ enabled: true, interval: "daily", retainDays: 14 });
      expect(config.keymap).toEqual({});
      expect(config.warnings).toEqual([]);
      expect(res.revision).toBeGreaterThan(0n);
    });

    it("a changed config streams on TOPIC_CONFIG and LiveStore keeps it", async () => {
      await setUp();
      const store = new LiveStore({ transport: target.transport, debounceMs: 0 });
      stores.push(store);
      store.start();
      await waitFor(() => store.getConfig().status === "ready" && store.getConnection().state === "live");
      expect(store.getConfig().config?.ui?.archiveDoneAfterDays).toBe(7);
      const before = store.getConfig().revision;

      await target.configure(3);
      await waitFor(() => store.getConfig().config?.ui?.archiveDoneAfterDays === 3);
      const snap = store.getConfig();
      expect(snap.config?.keymap).toEqual({ K: "move_up" });
      // What the file leaves out keeps its default.
      expect(snap.config?.ui?.reviewDay).toBe(Weekday.MONDAY);
      expect(snap.config?.backup).toMatchObject({ enabled: true, interval: "daily", retainDays: 14 });
      expect(snap.config?.exists).toBe(true);
      expect(snap.revision).toBeGreaterThan(before);

      const res = await system.getConfig({});
      expect(res.config?.ui?.archiveDoneAfterDays).toBe(3);
      expect(res.revision).toBeGreaterThanOrEqual(snap.revision);
    });

    it("a subscriber replaying from before two changes gets the current config on both", async () => {
      await setUp();
      const { revision } = await system.getConfig({});
      await target.configure(3);
      await target.configure(5);
      const watch = createClient(WatchService, target.transport);
      const ac = new AbortController();
      const seen: number[] = [];
      for await (const res of watch.subscribe({ sinceRevision: revision, topics: [Topic.CONFIG] }, { signal: ac.signal })) {
        if (res.message.case === "change" && res.message.value.resource.case === "config") {
          expect(res.message.value.id).toBe("config");
          seen.push(res.message.value.resource.value.ui?.archiveDoneAfterDays ?? 0);
        }
        if (res.message.case === "caughtUp") break;
      }
      ac.abort();
      expect(seen).toEqual([5, 5]);
    });
  });
}
