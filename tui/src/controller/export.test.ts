import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, rows } from "../fake.test-util.js";
import { defaultExportPath } from "./export.js";
import { setup, stopControllers } from "./setup.test-util.js";

afterEach(async () => {
  await stopControllers();
});

describe(":export", () => {
  it("names the file after the time, in the home directory", () => {
    expect(defaultExportPath(new Date(2026, 10, 3, 9, 5), "/home/ann")).toBe("/home/ann/todo-export-20261103-0905.jsonl");
  });

  it("writes every line beside the file, then renames it into place, and says how many Items", async () => {
    const fd = fakeDaemon(rows(2));
    // Either key order counts.
    fd.daemon.exportLines = ['{"format":"todo-tui-export"}', '{"type":"item","data":{}}', '{"data":{},"type":"item"}', '{"type":"view","data":{}}'];
    const writes: [string, string][] = [];
    const renames: [string, string][] = [];
    const exportFs = { writeFileSync: (p: string, t: string) => void writes.push([p, t]), renameSync: (a: string, b: string) => void renames.push([a, b]) };
    const t = await setup(fd, { controller: { exportFs } });
    await t.c.press(":export /tmp/x.jsonl<enter>");
    await vi.waitFor(() => expect(t.toast()).toBe("Exported 2 items to /tmp/x.jsonl"));
    expect(writes).toEqual([["/tmp/x.jsonl.tmp", `${fd.daemon.exportLines.join("\n")}\n`]]);
    expect(renames).toEqual([["/tmp/x.jsonl.tmp", "/tmp/x.jsonl"]]);
  });

  it("says why when it cannot", async () => {
    const t = await setup(fakeDaemon(rows(1)));
    await t.c.press(":export /tmp/x.jsonl<enter>");
    await vi.waitFor(() => expect(t.toast()).toMatch(/^Not exported: /));
  });
});
