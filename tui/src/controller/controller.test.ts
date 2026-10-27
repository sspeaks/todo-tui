import { Status, setStatus } from "@todo-tui/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, item, rows } from "../fake.test-util.js";
import { ERROR_TOAST_MS, INFO_TOAST_MS, MIDNIGHT_POLL_MS } from "./controller.js";
import { setup, stopControllers } from "./setup.test-util.js";

afterEach(async () => {
  await stopControllers();
  vi.useRealTimers();
});

describe("keys", () => {
  it("moves once per key of a burst (hard case 1)", async () => {
    const t = await setup();
    t.c.input("jjj", {});
    expect(t.selected()).toBe("row 3");
    t.c.input("k", {});
    expect(t.selected()).toBe("row 2");
    await t.c.press("G");
    expect(t.selected()).toBe("row 4");
    await t.c.press("gg");
    expect(t.selected()).toBe("row 0");
    await t.c.press("<down><down><up>");
    expect(t.selected()).toBe("row 1");
  });

  it("reads esc+key within Ink's 20 ms as esc then the key (hard case 3)", async () => {
    const t = await setup();
    await t.c.press(":");
    expect(t.state().mode.kind).toBe("command");
    t.c.input("j", { meta: true });
    await t.c.idle();
    expect(t.state().mode.kind).toBe("normal");
    expect(t.selected()).toBe("row 1");
  });

  it("drops an unbound continuation with a hint, and nothing runs (hard case 4)", async () => {
    const t = await setup();
    await t.c.press("d");
    expect(t.state().mode).toEqual({ kind: "normal", pending: ["d"] });
    expect(t.c.whichKey(["d"])).toBe("d… d Delete (confirm)");
    await t.c.press("j");
    expect(t.state().mode).toEqual({ kind: "normal", pending: [] });
    expect(t.state().toast?.text).toBe("d j: not bound · ? for help");
    expect(t.selected()).toBe("row 0");
    expect(t.daemon.ops.filter((o) => o.actor === "user")).toEqual([]);
    // dd asks first.
    await t.c.press("dd");
    expect(t.state().mode).toMatchObject({ kind: "confirm", text: 'Delete "row 0"? y/N' });
    await t.c.press("<esc>p9");
    expect(t.state().toast?.text).toBe("p 9: not bound · ? for help");
    // esc or ctrl-c cancels a prefix, and the key after it runs alone.
    await t.c.press("g<esc>j");
    expect(t.selected()).toBe("row 1");
    await t.c.press("g<ctrl+c>");
    expect(t.quit).not.toHaveBeenCalled();
  });

  it("opens help with ?, any F-key or :help, scrolls it, and closes it (hard case 6)", async () => {
    const t = await setup(fakeDaemon(rows(2)), { rows: 10 });
    for (const open of ["?", "<fkey>", ":help<enter>", ":h<enter>"]) {
      await t.c.press(open);
      expect(t.state().mode).toEqual({ kind: "help", scroll: 0 });
      await t.c.press("jj");
      expect(t.state().mode).toEqual({ kind: "help", scroll: 2 });
      await t.c.press("<esc>");
      expect(t.state().mode.kind).toBe("normal");
    }
    await t.c.press("?Gkq");
    expect(t.quit).not.toHaveBeenCalled();
    expect(t.state().mode.kind).toBe("normal");
    await t.c.press("?kkk");
    expect(t.state().mode).toEqual({ kind: "help", scroll: 0 });
  });

  it("runs help's two-key bindings: gg scrolls to the top, esc drops a pending g", async () => {
    const t = await setup(fakeDaemon(rows(2)), { rows: 10 });
    await t.c.press("?G");
    const bottom = t.state().mode;
    expect(bottom).toMatchObject({ kind: "help" });
    expect(bottom.kind === "help" && bottom.scroll).toBeGreaterThan(0);
    await t.c.press("gg");
    expect(t.state().mode).toMatchObject({ kind: "help", scroll: 0 });
    await t.c.press("Gg<esc>");
    expect(t.state().mode).toMatchObject({ kind: "help", scroll: bottom.kind === "help" ? bottom.scroll : -1 });
    await t.c.press("gj");
    expect(t.state().mode).toMatchObject({ kind: "help", scroll: bottom.kind === "help" ? bottom.scroll : -1 });
    await t.c.press("<esc>");
    expect(t.state().mode.kind).toBe("normal");
  });

  it("quits on q, :q, :quit and ctrl-c in normal mode; ctrl-c cancels the command line (hard case 9)", async () => {
    for (const keys of ["q", ":q<enter>", ":quit<enter>", ":qu<enter>", "<ctrl+c>"]) {
      const t = await setup();
      await t.c.press(keys);
      expect(t.quit, keys).toHaveBeenCalledTimes(1);
      expect(t.state().quitting).toBe(true);
      // Nothing runs once quitting.
      await t.c.press("j");
      expect(t.selected()).toBe("row 0");
    }
    const t = await setup();
    await t.c.press(":qu<ctrl+c>");
    expect(t.quit).not.toHaveBeenCalled();
    expect(t.state().mode.kind).toBe("normal");
  });

  it("runs commands, completes them, and says what is not one", async () => {
    const t = await setup();
    await t.c.press(":zz<enter>");
    expect(t.state().toast).toMatchObject({ level: "error", text: "Not a command: zz" });
    await t.c.press(":sync<enter>");
    expect(t.state().toast).toMatchObject({ level: "info", text: ":sync arrives with Jira sync in P2" });
    await t.c.press(":search milk<enter>");
    expect(t.state().toast?.text).toBe(":search is coming soon");
    await t.c.press(":re<tab>");
    expect(t.state().mode).toMatchObject({ kind: "command", line: { text: "refresh" } });
    const calls = t.daemon.calls.listItems;
    await t.c.press("<enter>");
    expect(t.state().toast?.text).toBe("Refreshed");
    await vi.waitFor(() => expect(t.daemon.calls.listItems).toBeGreaterThan(calls));
    // Backspace on an empty command line leaves it.
    await t.c.press(":x<backspace><backspace>");
    expect(t.state().mode.kind).toBe("normal");
    // A paste goes into the command line as one line, and is ignored in normal mode.
    await t.c.press(":");
    t.c.paste("quit\n");
    await t.c.idle();
    expect(t.state().mode).toMatchObject({ kind: "command", line: { text: "quit" } });
    await t.c.press("<esc>");
    t.c.paste("xxdd");
    await t.c.idle();
    // Never keys (hard case 7).
    expect(t.state().toast?.text).toBe("Paste ignored: press a to add");
    expect(t.selected()).toBe("row 0");
    expect(t.state().mode).toEqual({ kind: "normal", pending: [] });
    expect(t.daemon.ops.filter((o) => o.actor === "user")).toEqual([]);
  });
});

describe("selection", () => {
  it("falls back to the same index when the selected Item leaves, without a toast (hard case 14)", async () => {
    const t = await setup(fakeDaemon(rows(5)));
    await t.c.press("jj");
    const before = t.list();
    // Another client completes the selected row.
    const id = t.c.selectedId()!;
    await setStatus(t.clients().items, id, Status.DONE);
    await vi.waitFor(() => expect(t.selected()).toBe("row 3"));
    expect(t.list()).toMatchObject({ anchor: 2, offset: before.offset, topId: before.topId });
    expect(t.state().toast).toBeUndefined();
  });

  it("moves to the new last row, then to nothing, and the next row in is selected (hard case 12)", async () => {
    const [a, b] = rows(2);
    const t = await setup(fakeDaemon([a!, b!]));
    await t.c.press("G");
    t.daemon.remove(b!.id);
    await vi.waitFor(() => expect(t.selected()).toBe(a!.title));
    t.daemon.remove(a!.id);
    await vi.waitFor(() => expect(t.c.selectedId()).toBeUndefined());
    expect(t.list().selectedId).toBeUndefined();
    const c = item("new");
    t.put(c);
    await vi.waitFor(() => expect(t.c.selectedId()).toBe(c.id));
  });

  it("keeps the selection and the screen still when a row lands above the viewport (hard case 13)", async () => {
    const t = await setup(fakeDaemon(rows(2000)));
    await t.c.press("j".repeat(500));
    expect(t.selected()).toBe("row 500");
    const before = t.list();
    expect(before.offset).toBeGreaterThan(400);
    t.put(item("early bird", { createdAt: new Date(Date.UTC(2025, 0, 1)) }));
    await vi.waitFor(() => expect(t.list().offset).toBe(before.offset + 1));
    expect(t.list()).toMatchObject({ selectedId: before.selectedId, topId: before.topId });
  });

  it("pages by half a screen and stops at the ends", async () => {
    const t = await setup(fakeDaemon(rows(100)), { rows: 23 });
    // A 20-row list: half a page is 10 rows.
    await t.c.press("<ctrl+d>");
    expect(t.selected()).toBe("row 10");
    await t.c.press("<ctrl+d><ctrl+u>");
    expect(t.selected()).toBe("row 10");
    await t.c.press("<ctrl+u><ctrl+u>");
    expect(t.selected()).toBe("row 0");
    await t.c.press("G<ctrl+d>");
    expect(t.selected()).toBe("row 99");
    expect(t.list().offset).toBe(80);
  });

  it("reloads on ResyncRequired and keeps the selection by id, or by index (hard case 39)", async () => {
    const t = await setup(fakeDaemon(rows(5)));
    await t.c.press("jj");
    const [r0, r1, , r3, r4] = [...t.daemon.items.values()];
    // The database is replaced by an older one without row 2.
    t.daemon.restore(3n, [r0!, r1!, r3!, r4!]);
    t.dropStreams();
    await vi.waitFor(() => expect(t.selected()).toBe("row 3"));
    await t.c.press("k");
    t.daemon.restore(2n, [r0!, r1!, r3!]);
    t.dropStreams();
    await vi.waitFor(() => expect(t.store.view({ viewId: t.state().tab! }).getSnapshot().items).toHaveLength(3));
    expect(t.selected()).toBe("row 1");
  });
});

describe("tabs", () => {
  it("switches with 1–9 and [ ], wrapping, and restores each tab's selection and scroll (hard case 17)", async () => {
    const fd = fakeDaemon([...rows(60), item("next one", { status: Status.NEXT }), item("next two", { status: Status.NEXT })]);
    const t = await setup(fd);
    await t.c.press("j".repeat(40));
    const inbox = t.list();
    expect(t.selected()).toBe("row 40");
    await t.c.press("3");
    expect(t.tabIs("next")).toBe(true);
    await vi.waitFor(() => expect(t.selected()).toBe("next one"));
    await t.c.press("j");
    await t.c.press("1");
    expect(t.tabIs("inbox")).toBe(true);
    expect(t.list()).toEqual(inbox);
    expect(t.selected()).toBe("row 40");
    await t.c.press("3");
    await vi.waitFor(() => expect(t.selected()).toBe("next two"));
    // 9 with six views does nothing; ] on the last tab wraps to the first, [ back.
    await t.c.press("9");
    expect(t.tabIs("next")).toBe(true);
    await t.c.press("6]");
    expect(t.tabIs("inbox")).toBe(true);
    await t.c.press("[");
    expect(t.tabIs("archive")).toBe(true);
  });

  it("observes only the current tab's view", async () => {
    const t = await setup(fakeDaemon(rows(2)), { store: { viewGraceMs: 10 } });
    await t.c.press("3");
    await vi.waitFor(() => expect(t.store.view({ viewId: t.viewId("next") }).getSnapshot().status).toBe("ready"));
    // Inbox's view outlives the switch by the grace period only.
    await new Promise((r) => setTimeout(r, 30));
    const before = t.requests.length;
    t.put(item("change"));
    await vi.waitFor(() => expect(t.requests.length).toBeGreaterThan(before));
    await new Promise((r) => setTimeout(r, 30));
    expect(new Set(t.requests.slice(before).map((r) => r.viewId))).toEqual(new Set([t.viewId("next")]));
  });
});

describe("detail focus and the overlay", () => {
  it("focuses the detail beside the list when wide, and scrolls it within its content", async () => {
    const t = await setup(fakeDaemon(rows(3)), { columns: 120, rows: 20 });
    await t.c.press("<tab>");
    expect(t.state().focus).toBe("detail");
    const id = t.c.selectedId();
    t.c.setDetailLines(id, 25);
    await t.c.press("jjj");
    expect(t.state().detail).toEqual({ id, scroll: 3, lines: 25 });
    await t.c.press("G");
    expect(t.state().detail.scroll).toBe(25 - 17);
    expect(t.selected()).toBe("row 0");
    await t.c.press("<esc>");
    expect(t.state().focus).toBe("list");
    await t.c.press("j");
    expect(t.selected()).toBe("row 1");
    await t.c.press("<enter>");
    expect(t.state()).toMatchObject({ focus: "detail", overlay: { kind: "none" } });
  });

  it("opens the overlay with enter when narrow, closes it with esc, and resets it across the breakpoint (hard case 18)", async () => {
    const t = await setup(fakeDaemon(rows(3)), { columns: 100, rows: 20 });
    await t.c.press("<enter>");
    expect(t.state()).toMatchObject({ focus: "detail", overlay: { kind: "detail" } });
    await t.c.press("<esc>");
    expect(t.state()).toMatchObject({ focus: "list", overlay: { kind: "none" } });
    await t.c.press("<tab>");
    expect(t.state().overlay.kind).toBe("detail");
    await t.c.press("<tab>");
    expect(t.state().overlay.kind).toBe("none");
    await t.c.press("j<enter>");
    t.c.setSize(120, 20);
    expect(t.state()).toMatchObject({ focus: "list", overlay: { kind: "none" } });
    expect(t.selected()).toBe("row 1");
    await t.c.press("<tab>");
    t.c.setSize(100, 20);
    expect(t.state()).toMatchObject({ focus: "list", overlay: { kind: "none" } });
  });

  it("does not open the detail of an empty list", async () => {
    const t = await setup(fakeDaemon([item("x", { status: Status.NEXT })]));
    expect(t.list().selectedId).toBeUndefined();
    await t.c.press("<enter>");
    expect(t.state().overlay.kind).toBe("none");
  });
});

describe("toasts", () => {
  it("keeps info 4 s and errors 8 s or until the next key, newest winning", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"], shouldAdvanceTime: true });
    const t = await setup();
    t.c.toast("info", "one");
    t.c.toast("info", "two");
    expect(t.state().toast?.text).toBe("two");
    vi.advanceTimersByTime(INFO_TOAST_MS - 10);
    expect(t.state().toast?.text).toBe("two");
    vi.advanceTimersByTime(20);
    expect(t.state().toast).toBeUndefined();

    t.c.toast("error", "bad");
    vi.advanceTimersByTime(INFO_TOAST_MS + 10);
    expect(t.state().toast?.text).toBe("bad");
    vi.advanceTimersByTime(ERROR_TOAST_MS - INFO_TOAST_MS);
    expect(t.state().toast).toBeUndefined();

    t.c.toast("error", "bad again");
    await t.c.press("j");
    expect(t.state().toast).toBeUndefined();
    t.c.toast("info", "stays");
    await t.c.press("j");
    expect(t.state().toast?.text).toBe("stays");
  });
});

describe("midnight", () => {
  it("refreshes once the local date passes the daemon's today, once per today (hard case 47)", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    let now = new Date(Date.UTC(2026, 9, 26, 23, 59, 30));
    const clock = () => now;
    const fd = fakeDaemon(
      [
        item("due today", { status: Status.NEXT, dueDate: { year: 2026, month: 10, day: 26 } }),
        item("due tomorrow", { status: Status.NEXT, dueDate: { year: 2026, month: 10, day: 27 } }),
      ],
      { now: clock, timeZone: "UTC" },
    );
    const t = await setup(fd, { now: clock });
    await t.c.press("2");
    await vi.waitFor(() => expect(t.selected()).toBe("due today"));
    await vi.waitFor(() => expect(t.store.getCounts().today).toMatchObject({ day: 26 }));
    const refresh = vi.spyOn(t.store, "refresh");

    vi.advanceTimersByTime(MIDNIGHT_POLL_MS);
    expect(refresh).not.toHaveBeenCalled();

    // The laptop sleeps through midnight; the next poll catches up.
    now = new Date(Date.UTC(2026, 9, 27, 7, 0));
    vi.advanceTimersByTime(MIDNIGHT_POLL_MS);
    expect(refresh).toHaveBeenCalledTimes(1);
    const today = t.viewId("today");
    await vi.waitFor(() => expect(t.store.view({ viewId: today }).getSnapshot().items.map((i) => i.title)).toEqual(["due today", "due tomorrow"]));
    await vi.waitFor(() => expect(t.store.getCounts().today).toMatchObject({ day: 27 }));
    expect(t.selected()).toBe("due today");
    vi.advanceTimersByTime(MIDNIGHT_POLL_MS * 3);
    expect(refresh).toHaveBeenCalledTimes(1);
  });
});
