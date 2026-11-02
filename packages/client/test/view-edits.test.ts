import { describe } from "vitest";
import { FakeDaemon } from "../src/fake-daemon.js";
import { viewEditsConformance } from "./conformance/view-edits.js";

// The daemon runs the same suite in uds.integration.test.ts.
describe("FakeDaemon conformance", () => {
  viewEditsConformance(async () => ({ transport: new FakeDaemon().transport }));
});
