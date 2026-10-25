import { createClient } from "@connectrpc/connect";
import { describe } from "vitest";
import { ItemService } from "../src/gen/todo/v1/item_service_pb.js";
import { FakeDaemon } from "../src/fake-daemon.js";
import { undoRelationsConformance } from "./conformance/undo-relations.js";

// The daemon runs the same suite in uds.integration.test.ts.
describe("FakeDaemon conformance", () => {
  undoRelationsConformance(async () => {
    const daemon = new FakeDaemon();
    return {
      items: createClient(ItemService, daemon.transport),
      transport: daemon.transport,
      offStack: (id, patch) => void daemon.update(id, patch),
    };
  });
});
