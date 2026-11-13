import { createClient } from "@connectrpc/connect";
import { describe } from "vitest";
import { ItemService } from "../src/gen/todo/v1/item_service_pb.js";
import { PeopleService } from "../src/gen/todo/v1/people_service_pb.js";
import { FakeDaemon } from "../src/fake-daemon.js";
import { SEED, peopleConformance } from "./conformance/people.js";

// The daemon runs the same suite in uds.integration.test.ts.
describe("FakeDaemon conformance", () => {
  peopleConformance(async () => {
    const daemon = new FakeDaemon([]);
    for (const p of SEED) {
      daemon.putPerson({
        id: p.id,
        displayName: p.displayName,
        username: p.username,
        emails: p.email ? [p.email] : [],
        identities: [{ instanceId: "work-jira", accountId: p.accountId }],
      });
    }
    return { items: createClient(ItemService, daemon.transport), people: createClient(PeopleService, daemon.transport) };
  });
});
