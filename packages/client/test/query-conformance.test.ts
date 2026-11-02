import { describe } from "vitest";
import { FakeDaemon } from "../src/fake-daemon.js";
import { queryConformance } from "./conformance/query.js";

// The daemon runs the same suite, the grammar too, in uds.integration.test.ts.
describe("FakeDaemon conformance", () => {
  queryConformance(async () => ({ transport: new FakeDaemon().transport }), { grammar: false });
});
