import { MAX_CAPTURE_BYTES, Status, updateItem } from "@todo-tui/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { holding } from "./controller/setup.test-util.js";
import { fakeDaemon, item, rows } from "./fake.test-util.js";
import { cleanupTui, renderTui } from "./render.test-util.js";

const RED = "\x1b[31m";
const CYAN = "\x1b[36m";
const MAGENTA = "\x1b[35m";
const GREEN = "\x1b[32m";
const INVERSE = "\x1b[7m";

afterEach(async () => {
  await cleanupTui();
  vi.useRealTimers();
});

/** Monday 2026-10-26, noon UTC (the tests run with TZ=UTC). */
const NOON = new Date(Date.UTC(2026, 9, 26, 12));
const clock = () => NOON;
const dated = (init = rows(3)) => fakeDaemon(init, { now: clock, timeZone: "UTC" });

describe("quick-add on screen", () => {
  it("sits at the top of the list with its preview below: tokens coloured, the unapplied one red with its warning (hard case 40)", async () => {
    const t = renderTui({ daemon: dated(), columns: 100, rows: 12, now: clock });
    await t.live();
    await t.press("acall ^fri +2 #a ^bogus");
    await t.waitFrame((f) => expect(f).toContain("^bogus: not a date"));
    const [tabs, input, preview, first] = t.lines();
    expect(tabs).toMatch(/^1 Inbox 3/);
    expect(input).toMatch(/^Add: call \^fri \+2 #a \^bogus\s*$/);
    expect(preview).toBe("→ call ^bogus · +2 · ^Fri Oct 30 · #a · ^bogus: not a date");
    expect(first).toMatch(/^› ○\s+row 0/);
    expect(t.lines()).toHaveLength(12);
    const raw = t.raw().split("\n")[1] ?? "";
    expect(raw).toContain(`${CYAN}^fri`);
    expect(raw).toContain(`${MAGENTA}+2`);
    expect(raw).toContain(`${GREEN}#a`);
    expect(raw).toContain(`${RED}^bogus`);
    expect(t.raw().split("\n")[2]).toContain(`${RED} · ^bogus: not a date`);
    expect(t.lines()[10]).toMatch(/^ADD · ● live .*enter add/);
  });

  it("puts the highlight on the token exactly after an emoji (hard case 41)", async () => {
    const t = renderTui({ daemon: dated(), columns: 100, rows: 12, now: clock });
    await t.live();
    await t.press("a🎉 party ^fri");
    await t.waitFrame((f) => expect(f).toContain("→ 🎉 party · ^Fri Oct 30"));
    const raw = t.raw().split("\n")[1] ?? "";
    expect(raw).toContain(`🎉 party ${CYAN}^fri`);
  });

  it("shows the summary in full: title · +2 · ^date · #labels · ⧗person · >next", async () => {
    const t = renderTui({ daemon: dated(), columns: 120, rows: 12, now: clock });
    await t.live();
    await t.press('acall dentist +2 ^fri #a #b ~"the vendor" >next');
    await t.waitFrame((f) => expect(f).toContain("→ call dentist · +2 · ^Fri Oct 30 · #a #b · ⧗the vendor · >next"));
  });

  it("shows a hint when empty, and a red counter over 64 KiB (hard case 44)", async () => {
    const t = renderTui({ daemon: dated(), columns: 100, rows: 12, now: clock });
    await t.live();
    await t.press("a");
    await t.waitFrame((f) => expect(f).toMatch(/\n\+1–5 priority · \^date due/));
    t.controller.paste("y".repeat(MAX_CAPTURE_BYTES + 10));
    await t.waitFrame((f) => expect(f).toContain(`${(MAX_CAPTURE_BYTES + 10).toLocaleString("en")}/65,536 bytes: too long to add`));
    expect(t.raw().split("\n")[2]).toContain(RED);
    // The input shows its end, where the cursor is, on one line.
    expect(t.lines()[1]).toMatch(/^Add: y+\s?$/);
    expect(t.lines()).toHaveLength(12);
    await t.press("<enter>");
    await t.waitFrame((f) => expect(f).toContain("Too long to capture"));
  });

  it("toasts where a capture went on the message line, while quick-add stays open", async () => {
    const t = renderTui({ daemon: dated([item("n", { status: Status.NEXT })]), columns: 100, rows: 12, now: clock });
    await t.live();
    await t.press("3amilk<enter>");
    await t.waitFrame((f) => expect(f.split("\n")[11]).toBe('capture "milk" → Inbox'));
    expect(t.lines()[1]).toMatch(/^Add:\s*$/);
  });
});

describe("prompts on screen", () => {
  it("edits on the message line with the ParseDate preview above the status bar (hard case 45)", async () => {
    const t = renderTui({ daemon: dated(), columns: 100, rows: 12, now: clock });
    await t.live();
    await t.press("^");
    await t.waitFrame((f) => expect(f.split("\n").slice(-3)).toEqual([expect.stringMatching(/^enter clears the due date/), expect.stringMatching(/^EDIT · ● live/), "Due:  "]));
    await t.press("fri");
    await t.waitFrame((f) => expect(f.split("\n").slice(-3)).toEqual(["→ Fri Oct 30", expect.stringMatching(/^EDIT/), "Due: fri "]));
    expect(t.raw().split("\n")[11]).toContain(`${INVERSE} `);
    expect(t.lines()).toHaveLength(12);
    await t.press("<ctrl+u>nope");
    await t.waitFrame((f) => expect(f.split("\n")[9]).toBe("nope: not a date"));
    expect(t.raw().split("\n")[9]).toContain(RED);
    await t.press("<esc>");
    await t.waitFrame((f) => expect(f.split("\n")[10]).toMatch(/^NORMAL/));
  });

  it("shows a conflict: the server's value, and what enter and esc do (hard case 33)", async () => {
    const t = renderTui({ daemon: dated(), columns: 100, rows: 12, now: clock });
    await t.live();
    await t.press("i<ctrl+u>mine");
    await updateItem(t.clients().items, { id: t.controller.selectedId()!, title: "theirs" }, ["title"]);
    await t.press("<enter>");
    await t.waitFrame((f) =>
      expect(f.split("\n").slice(-3)).toEqual(["Changed elsewhere to “theirs” — enter overwrites · esc discards", expect.stringMatching(/^EDIT/), "Title: mine "]),
    );
  });

  it("strips a title's escape sequences from the prefilled text", async () => {
    const t = renderTui({ daemon: dated([item("red \x1b[31malert\x1b[0m")]), columns: 100, rows: 12, now: clock });
    await t.live();
    await t.press("i");
    await t.waitFrame((f) => expect(f.split("\n")[11]).toBe("Title: red alert "));
    expect(t.raw().split("\n")[11]).not.toContain(RED);
  });

  it("shows the status picker with the current status ticked and j/k's choice in inverse, and the dd confirm", async () => {
    const t = renderTui({ daemon: dated(), columns: 100, rows: 12, now: clock });
    await t.live();
    await t.press("sj");
    await t.waitFrame((f) => expect(f.split("\n")[11]).toBe("Status: i Inbox ✓ · n Next · p In progress · s Someday · d Done · c Cancelled"));
    expect(t.raw().split("\n")[11]).toMatch(new RegExp(`${INVERSE.replace("[", "\\[")}.*n.*Next`));
    await t.press("<esc>dd");
    await t.waitFrame((f) => expect(f.split("\n")[11]).toBe('Delete "row 0"? y/N'));
    await t.press("y");
    await t.waitFrame((f) => expect(f.split("\n")[11]).toBe('delete "row 0" · u undo'));
    await t.waitFrame((f) => expect(f).toContain(' · u: delete "row 0"'));
  });

  it("shows saving… once an action has run 250 ms", async () => {
    const h = holding(["setStatus"]);
    const t = renderTui({ daemon: dated(), columns: 100, rows: 12, now: clock, items: h.wrap });
    await t.live();
    h.hold();
    t.type("x");
    await vi.waitFor(() => expect(h.held.count).toBe(1));
    await new Promise((r) => setTimeout(r, 150));
    expect(t.frame()).not.toContain("saving…");
    await t.waitFrame((f) => expect(f.split("\n")[10]).toMatch(/^NORMAL · ● live · saving…/));
    h.release();
    await t.waitFrame((f) => expect(f).toContain('mark done "row 0" · u undo'));
    expect(t.frame()).not.toContain("saving…");
  });
});
