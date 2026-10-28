import {
  ArchiveReason,
  Code,
  ConnectError,
  Priority,
  RelationKind,
  Status,
  addRelation,
  deleteItem,
  setStatus,
  timestampFromDate,
} from "@todo-tui/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ERROR_TOAST_MS, INFO_TOAST_MS } from "./controller/controller.js";
import { DETAIL_FETCH_MS } from "./components/DetailPane.js";
import { fakeDaemon, item, rows } from "./fake.test-util.js";
import { cleanupTui, renderTui, stripAnsi } from "./render.test-util.js";

const RED = "\x1b[31m";
const YELLOW = "\x1b[33m";
const GREEN = "\x1b[32m";
const DIM = "\x1b[2m";
const INVERSE = "\x1b[7m";

afterEach(async () => {
  await cleanupTui();
  vi.useRealTimers();
});

/** Monday 2026-10-26, noon UTC (the tests run with TZ=UTC). */
const NOON = new Date(Date.UTC(2026, 9, 26, 12));
const clock = () => NOON;
const date = (day: number, month = 10) => ({ year: 2026, month, day });

describe("the shell", () => {
  it("renders tabs with counts, every row column, and the status bar, exactly the terminal's rows", async () => {
    const fd = fakeDaemon(
      [
        item("Call dentist", { priority: Priority.MEDIUM, dueDate: date(27) }),
        item("Overdue thing", { dueDate: date(24) }),
        item("Vendor contract", { waiting: { on: { ref: { case: "freeText", value: "dana" } }, since: date(23), followUpOn: date(26) } }),
        item("Overridden", { priority: Priority.LOW, localPriorityOverride: Priority.HIGHEST }),
        item("Next thing", { status: Status.NEXT }),
      ],
      { now: clock, timeZone: "UTC" },
    );
    const t = renderTui({ daemon: fd, columns: 100, rows: 12, now: clock });
    await t.live();
    await t.waitFrame((f) => expect(f).toContain("1 Inbox 4"));
    const [tabs, ...rest] = t.lines();
    expect(tabs).toMatch(/^1 Inbox 4 │ 2 Today 2 │ 3 Next 1 │ 4 Waiting 1 │ 5 Someday │ 6 Archive/);
    expect(t.lines()).toHaveLength(12);
    const line = (title: string) => rest.find((l) => l.includes(title)) ?? "";
    expect(line("Call dentist")).toMatch(/^› ○ \+3\s+Call dentist\s+\^Tom\s*$/);
    expect(line("Overdue thing")).toMatch(/\^Oct 24/);
    expect(line("Vendor contract")).toMatch(/⧗dana 3d/);
    expect(line("Overridden")).toMatch(/○ \+1\* Overridden/);
    const rawLine = (title: string) => t.raw().split("\n").find((l) => l.includes(title)) ?? "";
    expect(rawLine("Overdue thing")).toContain(`${RED}  ^Oct 24`);
    expect(rawLine("Vendor contract")).toContain(YELLOW);
    expect(rawLine("Call dentist")).not.toContain(RED);
    // The due column lines up across rows.
    expect(line("Call dentist").indexOf("^")).toBe(line("Overdue thing").indexOf("^"));
    expect(t.lines()[10]).toMatch(/^NORMAL · ● live .*\? help · : command · q quit$/);
  });

  it("keeps the Waiting view's waiting column when one name is long: the name is cut, the age kept", async () => {
    const on = (name: string) => ({ on: { ref: { case: "freeText" as const, value: name } }, since: date(23) });
    const fd = fakeDaemon(
      [
        item("vendor contract", { waiting: on("the vendor") }),
        item("passport", { waiting: on("jane doe") }),
        item("check invoice", { waiting: on("the account manager at Acme Corporation"), dueDate: date(30) }),
      ],
      { now: clock, timeZone: "UTC" },
    );
    const t = renderTui({ daemon: fd, columns: 120, rows: 12, now: clock });
    await t.live();
    await t.press("4");
    expect(t.ui.getState().tab).toBe(fd.viewId("waiting"));
    await t.waitFrame((f) => expect(f).toMatch(/^› ○\s+vendor contract/m));
    const line = (title: string) => t.lines().find((l) => l.includes(title)) ?? "";
    expect(line("vendor contract")).toMatch(/⧗the vendor 3d/);
    expect(line("passport")).toMatch(/⧗jane doe 3d/);
    expect(line("check invoice")).toMatch(/\^Fri\s+⧗the account manager.*… 3d/);
    // The column lines up across rows.
    expect(line("passport").indexOf("⧗")).toBe(line("check invoice").indexOf("⧗"));
  });

  it("dims archived rows and marks deferred ones (Archive)", async () => {
    const fd = fakeDaemon(
      [
        item("shelved", { archived: { at: timestampFromDate(NOON), reason: ArchiveReason.USER }, deferUntil: date(3, 11) }),
        item("finished", { status: Status.DONE }),
      ],
      { now: clock, timeZone: "UTC" },
    );
    const t = renderTui({ daemon: fd, now: clock });
    await t.live();
    t.type("6");
    await t.waitFrame((f) => expect(f).toContain("shelved"));
    expect(t.frame()).toMatch(/shelved\s+⏸Nov 3/);
    const raw = t.raw().split("\n");
    expect(raw.find((l) => l.includes("shelved"))).toContain(DIM);
    expect(raw.find((l) => l.includes("finished"))).not.toContain(DIM);
  });

  it("says what an empty view holds, by its seed, and selects the next row in (hard case 12)", async () => {
    const only = item("only one");
    const t = renderTui({ daemon: fakeDaemon([only]) });
    await t.live();
    await t.waitFrame((f) => expect(f).toContain("› ○     only one"));
    t.daemon.remove(only.id);
    await t.waitFrame((f) => expect(f).toContain("Inbox zero · a to add"));
    expect(t.selectedLine()).toBeUndefined();
    t.put(item("next in"));
    await t.waitFrame((f) => expect(f).toContain("› ○     next in"));
    t.type("2");
    await t.waitFrame((f) => expect(f).toContain("Nothing due today"));
    t.type("4");
    await t.waitFrame((f) => expect(f).toContain("Not waiting on anyone"));
  });

  it("switches tabs with digits and brackets, the current one highlighted", async () => {
    const t = renderTui({ daemon: fakeDaemon([item("in"), item("nx", { status: Status.NEXT })]) });
    await t.live();
    expect(t.raw()).toMatch(/\x1b\[7m(\x1b\[1m)?1 Inbox 1/);
    t.type("3");
    await t.waitFrame((f) => expect(f).toContain("› ○     nx"));
    expect(t.raw()).toMatch(/\x1b\[7m(\x1b\[1m)?3 Next 1/);
    // The new-row highlight starts over per tab: Next's rows are not new.
    expect(t.raw().split("\n").find((l) => l.includes("nx"))).not.toContain(GREEN);
    t.type("]]]]");
    await t.waitFrame((f) => expect(f).toContain("› ○     in"));
  });

  it("moves once per key of a chunk Ink delivers whole (hard case 1)", async () => {
    const t = renderTui({ daemon: fakeDaemon(rows(5)) });
    await t.live();
    await t.waitFrame((f) => expect(f).toContain("row 4"));
    t.type("jjj");
    await t.waitFrame(() => expect(t.selectedLine()).toContain("row 3"));
    t.type("\x1b[A");
    await t.waitFrame(() => expect(t.selectedLine()).toContain("row 2"));
  });

  it("shows ◌ and dims the list while not live, and comes back (reconnect)", async () => {
    const t = renderTui({ daemon: fakeDaemon([item("A")]) });
    await t.live();
    t.daemon.down = true;
    t.dropStreams();
    await t.waitFrame((f) => expect(f).toMatch(/◌ reconnecting/));
    expect(t.raw().split("\n").find((l) => l.includes(" A"))).toContain(DIM);
    t.daemon.down = false;
    t.put(item("Missed while down"));
    await t.waitFrame((f) => expect(f).toContain("Missed while down"));
    await t.live();
  });

  it("shows the undo hint from the undo state", async () => {
    const a = item("tidy desk");
    const t = renderTui({ daemon: fakeDaemon([a, item("other")]) });
    await t.live();
    await setStatus(t.clients().items, a.id, Status.DONE);
    await t.waitFrame((f) => expect(f).toContain(`u: mark done "tidy desk"`));
  });

  it("highlights a newly appeared row briefly", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"], shouldAdvanceTime: true });
    const t = renderTui({ daemon: fakeDaemon([item("Old")]) });
    await t.live();
    const line = (title: string) => t.raw().split("\n").find((l) => l.includes(title)) ?? "";
    expect(line("Old")).not.toContain(GREEN);
    t.put(item("New"));
    await t.waitFrame((f) => expect(f).toContain("New"));
    expect(line("New")).toContain(GREEN);
    vi.advanceTimersByTime(2000);
    await vi.waitFor(() => expect(line("New")).not.toContain(GREEN));
  });

  it("shows a view's load error on one line, escapes stripped", async () => {
    const fd = fakeDaemon(rows(2));
    fd.daemon.onListItems = () => {
      throw new ConnectError("bad \x1b[32mfilter\nsecond line", Code.InvalidArgument);
    };
    const t = renderTui({ daemon: fd, columns: 100, rows: 10 });
    await t.waitFrame((f) => expect(f).toContain("Could not load this view"));
    expect(t.lines()[1]).toBe("Could not load this view: [invalid_argument] bad filter second line");
    expect(t.raw().split("\n")[1]).not.toContain("\x1b[32m");
  });

  it("quits on q", async () => {
    const t = renderTui();
    await t.live();
    t.type("q");
    await t.app.waitUntilExit();
    expect(t.ui.getState().quitting).toBe(true);
  });
});

describe("sizes", () => {
  it("renders only the visible rows, never more lines than the terminal, through G gg ctrl-d ctrl-u j (hard case 16)", async () => {
    const t = renderTui({ daemon: fakeDaemon(rows(2000)), columns: 100, rows: 30 });
    await t.live();
    await t.waitFrame((f) => expect(f).toContain("row 26"));
    const listRows = () => t.lines().filter((l) => /row \d+/.test(l));
    expect(listRows()).toHaveLength(27);
    const steps: [string, string][] = [
      ["G", "row 1999"],
      ["gg", "row 0"],
      ["\x04", "row 13"],
      ["\x04", "row 26"],
      ["\x15", "row 13"],
      ["jjjjj", "row 18"],
    ];
    for (const [keys, selected] of steps) {
      t.type(keys);
      await t.waitFrame(() => expect(t.selectedLine()).toMatch(new RegExp(`${selected}\\s*$`)));
      expect(listRows()).toHaveLength(27);
    }
    for (const f of t.term.stdout.frames) expect(f.split("\n").length).toBeLessThanOrEqual(30);
  });

  it("keeps the screen still when a row lands above it (hard case 13)", async () => {
    const t = renderTui({ daemon: fakeDaemon(rows(2000)) });
    await t.live();
    await t.press("j".repeat(500));
    await t.waitFrame(() => expect(t.selectedLine()).toMatch(/row 500\s*$/));
    const before = t.lines().slice(1, 28);
    t.put(item("early bird", { createdAt: new Date(Date.UTC(2025, 0, 1)) }));
    await t.waitFrame((f) => expect(f).toContain("1 Inbox 2001"));
    expect(t.lines().slice(1, 28)).toEqual(before);
    await t.press("gg");
    await t.waitFrame((f) => expect(f).toContain("early bird"));
  });

  it("splits at 110 columns; narrower, enter opens the detail overlay; the breakpoint resets it (hard case 18)", async () => {
    const t = renderTui({ daemon: fakeDaemon(rows(50)), columns: 120, rows: 40 });
    await t.live();
    await t.waitFrame((f) => expect(f).toMatch(/row 0 .*│ row 0/));
    expect(t.frame()).toContain("Status    ○ Inbox");
    t.resize(100, 40);
    await t.waitFrame((f) => expect(f).not.toContain("Status    ○ Inbox"));
    expect(t.lines()[1]).toMatch(/^› ○ {5}row 0\s*$/);
    t.type("\r");
    await t.waitFrame((f) => expect(f).toContain("Status    ○ Inbox"));
    expect(t.frame()).not.toContain("row 1");
    t.resize(120, 40);
    await t.waitFrame((f) => expect(f).toMatch(/row 1 .*│/));
    expect(t.ui.getState().overlay.kind).toBe("none");
    expect(t.selectedLine()).toMatch(/› ○ {5}row 0/);
    // Shrinking the rows keeps the selection in view.
    await t.press("j".repeat(30));
    t.resize(120, 12);
    await t.waitFrame((f) => expect(f.split("\n")).toHaveLength(12));
    expect(t.selectedLine()).toMatch(/row 30/);
  });

  it("shows only 'Terminal too small' below 40×8 (hard case 19)", async () => {
    const t = renderTui({ daemon: fakeDaemon(rows(3)), columns: 30, rows: 5 });
    await t.waitFrame((f) => expect(f).toBe("Terminal too small (need 40×8)"));
    t.resize(20, 5);
    await t.waitFrame((f) => expect(f).toMatch(/^Terminal too small.*…$/));
    t.resize(39, 30);
    await t.waitFrame((f) => expect(f).toBe("Terminal too small (need 40×8)"));
    for (const f of t.term.stdout.frames) expect(stripAnsi(f).split("\n").filter(Boolean)).toHaveLength(1);
    t.resize(40, 8);
    await t.waitFrame((f) => expect(f.split("\n")).toHaveLength(8));
  });

  it("puts a long title with wide characters and escapes on one truncated line, escapes stripped (hard case 20)", async () => {
    const title = `\x1b[31mred\x07 🎉 日本語 ${"long ".repeat(60)}`;
    const t = renderTui({ daemon: fakeDaemon([item(title)]), columns: 80, rows: 10 });
    await t.live();
    await t.waitFrame((f) => expect(f).toContain("red 🎉 日本語 long"));
    const raw = t.raw().split("\n").find((l) => l.includes("日本語")) ?? "";
    expect(raw).not.toContain(RED);
    expect(raw).not.toContain("\x07");
    const line = stripAnsi(raw);
    expect(line).toMatch(/…$/);
    expect(t.lines()).toHaveLength(10);
  });
});

describe("help, commands and toasts", () => {
  it("opens help on ?, an F-key or :help, generated from the keymap, scrolled with j/k (hard case 6)", async () => {
    const t = renderTui({ daemon: fakeDaemon(rows(2)), rows: 12 });
    await t.live();
    for (const open of ["?", "\x1bOP", "\x1b[15~", ":help\r"]) {
      t.type(open);
      await t.waitFrame((f) => expect(f).toContain("Help · j/k scroll · esc close"));
      expect(t.frame()).toMatch(/j ↓\s+Next row/);
      expect(t.lines()).toHaveLength(12);
      await t.press("<esc>");
      await t.waitFrame((f) => expect(f).not.toContain("Help ·"));
    }
    t.type("?");
    await t.waitFrame((f) => expect(f).toContain("Help ·"));
    t.type("G");
    await t.waitFrame((f) => expect(f).toContain("the next Nov 2"));
    expect(t.frame()).not.toContain("Help ·");
    expect(t.lines()[10]).toMatch(/^HELP · ● live/);
  });

  it("drops the key hints before the mode, the connection or a pending prefix when they do not fit", async () => {
    const t = renderTui({ daemon: fakeDaemon(rows(2)), columns: 80, rows: 10 });
    await t.live();
    const status = () => t.lines()[8] ?? "";
    expect(status()).toMatch(/^NORMAL · ● live\s+\? help · : command · q quit$/);
    t.type("p");
    await t.waitFrame(() => expect(status()).toMatch(/^NORMAL · ● live · p… 1–5 Set the priority · 0 Clear the priority\s*$/));
    await t.press("<esc>");
    t.resize(40, 10);
    await t.waitFrame(() => expect(status()).toMatch(/^NORMAL · ● live\s*$/));
    t.daemon.down = true;
    t.dropStreams();
    await t.waitFrame(() => expect(status()).toMatch(/^NORMAL · ◌ reconnecting/));
  });

  it("edits a command on the message line and reports what is not a command", async () => {
    const t = renderTui({ daemon: fakeDaemon(rows(2)), rows: 10 });
    await t.live();
    t.type(":ref");
    // The cursor is an inverse cell after the text.
    await t.waitFrame((f) => expect(f.split("\n")[9]).toBe(":ref "));
    expect(t.raw()).toContain(`${INVERSE} `);
    t.type("\t");
    await t.waitFrame((f) => expect(f.split("\n")[9]).toBe(":refresh "));
    await t.press("<ctrl+u>zz<enter>");
    await t.waitFrame((f) => expect(f.split("\n")[9]).toBe("Not a command: zz"));
    expect(t.raw().split("\n")[9]).toContain(RED);
  });

  it("shows toasts on one line: info for 4 s, errors for 8 s or until the next key (hard case 48)", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"], shouldAdvanceTime: true });
    const t = renderTui({ daemon: fakeDaemon(rows(2)), rows: 10 });
    await t.live();
    t.controller.toast("info", "x".repeat(300));
    await t.waitFrame((f) => expect(f.split("\n")[9]).toMatch(/^x+…$/));
    expect(t.lines()).toHaveLength(10);
    vi.advanceTimersByTime(INFO_TOAST_MS + 10);
    await t.waitFrame((f) => expect(f.split("\n")[9]?.trim()).toBe(""));
    t.controller.toast("error", "it broke");
    await t.waitFrame((f) => expect(f).toContain("it broke"));
    vi.advanceTimersByTime(ERROR_TOAST_MS - 100);
    expect(t.frame()).toContain("it broke");
    t.type("j");
    await t.waitFrame((f) => expect(f).not.toContain("it broke"));
    for (const f of t.term.stdout.frames) expect(f.split("\n").length).toBeLessThanOrEqual(10);
  });
});

describe("the detail pane", () => {
  it("shows fields, the parent (fetched), subtasks and relations from this Item's side", async () => {
    const fd = fakeDaemon([], { now: clock, timeZone: "UTC" });
    const done = fd.put(item("parent done", { status: Status.DONE }));
    const parent = fd.put(
      item("parent open", {
        labels: ["home", "x\x1b[31m"],
        dueDate: date(30),
        waiting: { on: { ref: { case: "freeText", value: "dana" } }, since: date(20), followUpOn: date(30), reason: "quote" },
        description: "line one\nline \x1b[31mtwo\x07",
      }),
    );
    fd.put(item("kid one", { parentId: parent.id }));
    fd.put(item("kid two", { parentId: parent.id, status: Status.DONE }));
    fd.put(item("child of done", { parentId: done.id }));
    const blocker = fd.put(item("blocker"));
    const gone = fd.put(item("gone", { status: Status.SOMEDAY }));
    const items = fd.clients().items;
    await addRelation(items, { fromId: blocker.id, toId: parent.id, kind: RelationKind.BLOCKS });
    await addRelation(items, { fromId: parent.id, toId: gone.id, kind: RelationKind.DUPLICATES });
    await deleteItem(items, gone.id);
    const t = renderTui({ daemon: fd, columns: 130, rows: 40, now: clock });
    await t.live();
    await t.waitFrame((f) => expect(f).toContain("Subtasks (2)"));
    const f = t.frame();
    expect(f).toMatch(/│ parent open/);
    expect(f).toMatch(/Due {7}Fri Oct 30 2026 \(in 4 days\)/);
    expect(f).toMatch(/Waiting {3}dana · since Oct 20 \(6d\) · follow up Fri\s+│\s+· quote/);
    expect(f).toMatch(/Labels {4}#home #x/);
    expect(f).toMatch(/ {2}○ kid one/);
    expect(f).toMatch(/ {2}● kid two/);
    expect(f).toMatch(/Blocked by ○ blocker/);
    expect(f).toMatch(/Duplicates ○ gone \(deleted\)/);
    expect(f).toContain("line one");
    expect(f).toContain("line two");
    expect(f).toMatch(/Created {3}2026-01-0\d \d\d:\d\d\s/);
    expect(f).toMatch(/Updated {3}2026-10-26 12:00\s/);
    const raw = t.raw();
    expect(raw).not.toContain("\x07");
    expect(raw.split("\n").find((l) => l.includes("line two"))).not.toContain(RED);

    await t.press("j");
    await t.waitFrame((x) => expect(x).toMatch(/Parent {4}○ parent open/));
    // This row's parent is done, so in no view: it is fetched.
    expect(t.store.getItem(done.id)).toBeUndefined();
    t.type("j");
    await t.waitFrame((x) => expect(x).toMatch(/Parent {4}● parent done/));
  });

  it("fetches only once the selection settles", async () => {
    const fd = fakeDaemon(rows(30));
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.waitFrame((f) => expect(f).toMatch(/│ row 0/));
    await new Promise((r) => setTimeout(r, DETAIL_FETCH_MS * 2));
    const before = fd.requests.length;
    // Ten key events, each rendered before the next (one chunk would be a single change), all within the debounce.
    for (let i = 0; i < 10; i++) {
      t.type("j");
      await new Promise((r) => setImmediate(r));
    }
    await t.waitFrame((f) => expect(f).toMatch(/│ row 10/));
    await new Promise((r) => setTimeout(r, DETAIL_FETCH_MS * 3));
    // One subtasks query, for the Item the selection settled on.
    const subtasks = fd.requests.slice(before).filter((r) => r.filter?.allOf.length);
    expect(subtasks).toHaveLength(1);
  });

  it("shows a parent and a relation the store holds at once; only fetches wait for the selection to settle", async () => {
    const fd = fakeDaemon([]);
    const parent = fd.put(item("the parent"));
    const child = fd.put(item("the child", { parentId: parent.id }));
    await addRelation(fd.clients().items, { fromId: parent.id, toId: child.id, kind: RelationKind.BLOCKS });
    const t = renderTui({ daemon: fd, columns: 120, rows: 30 });
    await t.live();
    await t.waitFrame((f) => expect(f).toMatch(/│ the parent/));
    const from = t.term.stdout.frames.length;
    t.type("j");
    await t.waitFrame((f) => expect(f).toMatch(/│ the child/));
    const shown = t.term.stdout.frames.slice(from).map(stripAnsi).filter((f) => /│ the child/.test(f));
    expect(shown.length).toBeGreaterThan(0);
    for (const f of shown) {
      expect(f).toMatch(/Parent {4}○ the parent/);
      expect(f).toMatch(/Blocked by ○ the parent/);
    }
  });

  it("scrolls with j/k once focused with tab", async () => {
    const description = Array.from({ length: 40 }, (_, i) => `para ${i}`).join("\n");
    const t = renderTui({ daemon: fakeDaemon([item("long one", { description })]), columns: 120, rows: 20 });
    await t.live();
    await t.waitFrame((f) => expect(f).toMatch(/│ long one/));
    t.type("\t");
    await t.waitFrame((f) => expect(f).toMatch(/^DETAIL/m));
    t.type("jjjjj");
    await t.waitFrame((f) => expect(f).not.toMatch(/│ long one/));
    t.type("G");
    await t.waitFrame((f) => expect(f).toContain("para 39"));
    expect(t.lines()).toHaveLength(20);
    await t.press("<esc>");
    await t.waitFrame((f) => expect(f).toMatch(/^NORMAL/m));
  });
});
