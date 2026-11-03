import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, rows } from "../fake.test-util.js";
import { setup, stopControllers } from "./setup.test-util.js";

afterEach(async () => {
  await stopControllers();
});

const TEXT = `L48:\n${Array.from({ length: 40 }, (_, i) => `- line ${i}`).join("\n")}\nN48:\n- (nothing)\n`;

describe(":l48", () => {
  it("opens the daemon's report; j/k scroll, y copies it, esc closes", async () => {
    const fd = fakeDaemon(rows(2));
    fd.daemon.setActivityReport({ text: TEXT });
    const copied: string[] = [];
    const t = await setup(fd, { controller: { clipboard: (text) => (copied.push(text), "pbcopy") } });
    await t.c.press(":l48<enter>");
    await vi.waitFor(() => expect(t.mode().kind).toBe("report"));
    expect(t.mode()).toMatchObject({ kind: "report", scroll: 0, text: TEXT });
    await t.c.press("jj");
    expect(t.mode()).toMatchObject({ kind: "report", scroll: 2 });
    await t.c.press("k");
    expect(t.mode()).toMatchObject({ scroll: 1 });
    await t.c.press("y");
    expect(copied).toEqual([TEXT]);
    expect(t.toast()).toBe("Copied L48 / N48 (pbcopy)");
    // Normal keys do nothing here: x does not mark anything done.
    await t.c.press("x");
    expect(t.userOps()).toEqual([]);
    await t.c.press("<esc>");
    expect(t.mode()).toEqual({ kind: "normal", pending: [] });
  });

  it("says so when nothing could copy, and passes since on", async () => {
    const fd = fakeDaemon(rows(1));
    const t = await setup(fd, { controller: { clipboard: () => undefined } });
    // FakeDaemon answers its fixture whatever since says; the daemon's
    // reading of it is tested in crates/todo-daemon/tests/server.rs.
    await t.c.press(":l48 thu<enter>");
    await vi.waitFor(() => expect(t.mode().kind).toBe("report"));
    await t.c.press("<esc>");
    expect(t.mode()).toEqual({ kind: "normal", pending: [] });
    await t.c.press(":l48 2026-10-29<enter>");
    await vi.waitFor(() => expect(t.mode().kind).toBe("report"));
    await t.c.press("y");
    expect(t.toast()).toBe("Could not copy: no pbcopy, wl-copy or xclip");
  });
});
