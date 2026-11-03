import { createClient } from "@connectrpc/connect";
import { describe } from "vitest";
import { ItemService } from "../src/gen/todo/v1/item_service_pb.js";
import { FakeDaemon } from "../src/fake-daemon.js";
import { meetingConformance } from "./conformance/meeting.js";

// The daemon runs the same suite in uds.integration.test.ts.
describe("FakeDaemon conformance", () => {
  meetingConformance(async () => {
    const daemon = new FakeDaemon([]);
    return { items: createClient(ItemService, daemon.transport) };
  });
});
