import { createClient } from "@connectrpc/connect";
import { describe } from "vitest";
import { ItemService } from "../src/gen/todo/v1/item_service_pb.js";
import { FakeDaemon } from "../src/fake-daemon.js";
import { FILTER_ZONE, filterViewsConformance } from "./conformance/filter-views.js";

// The daemon runs the same suite in uds.integration.test.ts.
describe("FakeDaemon conformance", () => {
  filterViewsConformance(async () => {
    const daemon = new FakeDaemon([], { timeZone: FILTER_ZONE });
    return { items: createClient(ItemService, daemon.transport), transport: daemon.transport, timeZone: FILTER_ZONE };
  });
});
