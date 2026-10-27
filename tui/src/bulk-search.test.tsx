import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, item, rows } from "./fake.test-util.js";
import { cleanupTui, renderTui } from "./render.test-util.js";

const MAGENTA = "\x1b[35m";

afterEach(async () => {
  await cleanupTui();
  vi.useRealTimers();
});

const statusLine = (lines: string[]) => lines.at(-2) ?? "";

describe("marks on screen", () => {
  it("shows a • on marked rows, in magenta, and the count in the status bar", async () => {
    const t = renderTui({ daemon: fakeDaemon(rows(4)), columns: 100, rows: 10 });
    await t.live();
    await t.press("mm");
    await t.waitFrame((f) => expect(f).toContain("2 marked"));
    const [, first, second, third] = t.lines();
    expect(first).toMatch(/^ •○\s+row 0/);
    expect(second).toMatch(/^ •○\s+row 1/);
    expect(third).toMatch(/^› ○\s+row 2/);
    expect(t.raw().split("\n")[1]).toContain(MAGENTA);
    expect(statusLine(t.lines())).toMatch(/^NORMAL · ● live · 2 marked/);
    // V: the mode says so, and how it ends.
    await t.press("V");
    await t.waitFrame((f) => expect(f).toContain("3 marked"));
    expect(statusLine(t.lines())).toMatch(/^VISUAL · ● live · 3 marked .*V or esc ends/);
  });

  it("says “N marked (H hidden)” while / hides some, and keeps the tab badge un-narrowed (hard cases 24, 49)", async () => {
    const t = renderTui({ daemon: fakeDaemon([item("bread"), item("milk"), item("oat milk")]), columns: 100, rows: 10 });
    await t.live();
    await t.press("mm/milk");
    await t.controller.previewIdle();
    await t.waitFrame((f) => expect(f).toContain("/milk · 2"));
    expect(t.lines()[0]).toMatch(/^1 Inbox 3/);
    expect(statusLine(t.lines())).toMatch(/^SEARCH · ● live · 2 marked \(1 hidden\) · \/milk · 2/);
    expect(t.lines().at(-1)).toMatch(/^\/milk/);
    expect(t.frame()).not.toContain("bread");
    await t.press("<enter>");
    await t.waitFrame((f) => expect(statusLine(f.split("\n"))).toMatch(/^NORMAL · .*\/milk · 2/));
    await t.press("/<ctrl+u>zzz");
    await t.controller.previewIdle();
    await t.waitFrame((f) => expect(f).toContain("Nothing here matches “zzz” · esc clears"));
  });
});

describe("the Search tab on screen", () => {
  it("sits after the views, unnumbered, with its count, and says when nothing matches", async () => {
    const t = renderTui({ daemon: fakeDaemon([item("milk"), item("oat milk")]), columns: 120, rows: 10 });
    await t.live();
    await t.press(":search milk<enter>");
    await t.waitFrame((f) => expect(f.split("\n")[0]).toMatch(/Archive │ Search: milk 2$/));
    await t.press(":search zzz<enter>");
    await t.waitFrame((f) => expect(f).toContain("No Items match “zzz”"));
    expect(t.lines()[0]).toMatch(/Search: zzz$/);
  });
});

describe("A on screen", () => {
  it("names the parent under the line", async () => {
    const t = renderTui({ daemon: fakeDaemon([item("plan the trip")]), columns: 100, rows: 10 });
    await t.live();
    await t.press("A");
    await t.waitFrame((f) => expect(f.split("\n")[2]).toMatch(/^child of “plan the trip” · \+1–5 priority/));
  });
});
