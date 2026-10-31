import { createClient } from "@connectrpc/connect";
import { describe, expect } from "vitest";
import { ItemService } from "../src/gen/todo/v1/item_service_pb.js";
import { FakeDaemon } from "../src/fake-daemon.js";
import { RECURRENCE_ZONE, recurrenceConformance } from "./conformance/recurrence.js";

// The daemon runs the same suite in uds.integration.test.ts.
describe("FakeDaemon conformance", () => {
  recurrenceConformance(async () => {
    const daemon = new FakeDaemon([], { timeZone: RECURRENCE_ZONE });
    return { items: createClient(ItemService, daemon.transport), timeZone: RECURRENCE_ZONE };
  });
});

// Each test starts a millisecond before midnight in the zone, and the
// daemon's clock moves on once the suite has read today: the day changes
// after the test reads it, before the daemon does. Its second run is on
// the new day.
describe("FakeDaemon conformance across midnight", () => {
  /** 00:00 on 2026-10-31 in Asia/Kathmandu. */
  const MIDNIGHT = Date.UTC(2026, 9, 30, 18, 15);
  let test: string | undefined;
  let ms = 0;
  let read = false;
  recurrenceConformance(async () => {
    const name = expect.getState().currentTestName;
    if (name !== test) [test, ms, read] = [name, MIDNIGHT - 1, false];
    const now = () => {
      read = true;
      return ms;
    };
    const daemonNow = () => {
      if (read) ms = Math.max(ms + 1, MIDNIGHT);
      return new Date(ms);
    };
    const daemon = new FakeDaemon([], { timeZone: RECURRENCE_ZONE, now: daemonNow });
    return { items: createClient(ItemService, daemon.transport), timeZone: RECURRENCE_ZONE, now };
  });
});
