import { describe } from "vitest";
import { FakeDaemon } from "../src/fake-daemon.js";
import { configConformance } from "./conformance/config.js";

// The daemon runs the same suite in uds.integration.test.ts.
describe("FakeDaemon conformance", () => {
  configConformance(async () => {
    const daemon = new FakeDaemon();
    return {
      transport: daemon.transport,
      configure: async (days) => {
        daemon.setConfig({ ui: { archiveDoneAfterDays: days }, keymap: { K: "move_up" } });
      },
    };
  });
});
