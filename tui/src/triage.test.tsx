import { mkdtempSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
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

describe("wide text in a line", () => {
  it("keeps the cursor and the end of a long CJK line in view, in quick-add and a prompt", async () => {
    const t = renderTui({ daemon: dated(), columns: 70, rows: 12, now: clock });
    await t.live();
    const cjk = `${"日本語のタイトルをとても長く入力してみるテストです".repeat(2)} END`;
    await t.press(`a${cjk.replaceAll(" ", "<space>")}`);
    await t.waitFrame((f) => expect(f.split("\n")[1]).toMatch(/^Add: .* END\s*$/));
    await t.press("<esc>i<ctrl+u>");
    await t.press(cjk.replaceAll(" ", "<space>"));
    await t.waitFrame((f) => expect(f.split("\n").at(-1)).toMatch(/ END\s*$/));
  });
});

describe("alt keys through Ink's parser", () => {
  it("keeps quick-add's text on Alt+Left (CSI 1;3D), Option+Left (ESC b) and Alt+Backspace; a typed esc still closes", async () => {
    const t = renderTui({ daemon: dated(), columns: 100, rows: 12, now: clock });
    await t.live();
    await t.press("abuy milk tomorrow");
    t.type("\x1b[1;3D");
    await vi.waitFor(() => expect(t.ui.getState().mode).toMatchObject({ kind: "quickAdd", line: { cursor: 9 } }));
    t.type("\x1bb");
    await vi.waitFor(() => expect(t.ui.getState().mode).toMatchObject({ kind: "quickAdd", line: { cursor: 4 } }));
    t.type("\x1b[1;3C");
    t.type("\x1b\x7f");
    await vi.waitFor(() => expect(t.ui.getState().mode).toMatchObject({ kind: "quickAdd", line: { text: "buy  tomorrow", cursor: 4 } }));
    await t.waitFrame((f) => expect(f.split("\n")[1]).toMatch(/^Add: buy {2}tomorrow/));
    t.type("\x1b");
    await vi.waitFor(() => expect(t.ui.getState().mode.kind).toBe("normal"));
  });
});

describe("E on screen", () => {
  it("shows the whole name of the file a kept description went to, however narrow (Decision 6)", async () => {
    const stateDir = mkdtempSync(join(tmpdir(), "todo-tui-test-"));
    try {
      const fd = fakeDaemon([item("pay rent", { description: "on the 1st" })]);
      const id = [...fd.daemon.items.keys()][0] ?? "";
      const editor = async () => {
        // Meanwhile another client changes the description.
        await updateItem(fd.clients().items, { id, description: "theirs" }, ["description"]);
        return { ok: true as const, text: "mine\n" };
      };
      const t = renderTui({ daemon: fd, columns: 100, rows: 10, controller: { editor, stateDir } });
      await t.live();
      await t.press("E");
      const [file = ""] = readdirSync(join(stateDir, "recovered"));
      expect(file).toMatch(new RegExp(`^${id}-\\d{8}T\\d{6}Z\\.md$`));
      // Longer than the line: cut in the middle, the reason and the file's name both shown.
      expect(t.ui.getState().toast?.text.length).toBeGreaterThan(100);
      await t.waitFrame((f) => expect(f.split("\n").at(-1)).toMatch(new RegExp(`^Description changed elsewhere; .*….*${file}$`)));
    } finally {
      rmSync(stateDir, { recursive: true, force: true });
    }
  });
});
