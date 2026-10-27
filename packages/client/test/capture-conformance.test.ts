import { createClient } from "@connectrpc/connect";
import { describe } from "vitest";
import { ItemService } from "../src/gen/todo/v1/item_service_pb.js";
import { FakeDaemon } from "../src/fake-daemon.js";
import { CAPTURE_ZONE, captureConformance } from "./conformance/capture.js";

// The daemon runs the same suite in uds.integration.test.ts.
describe("FakeDaemon conformance", () => {
  captureConformance(async () => {
    const daemon = new FakeDaemon([], { timeZone: CAPTURE_ZONE });
    return { items: createClient(ItemService, daemon.transport), timeZone: CAPTURE_ZONE };
  });
});
