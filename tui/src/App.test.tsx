import { ItemField, LiveStore, LiveStoreProvider, Priority, Status } from "@todo-tui/client";
import { render } from "ink-testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "./App.js";
import { fakeDaemon, item } from "./fake.test-util.js";

const stores: LiveStore[] = [];
const stripAnsi = (s: string) => s.replace(/\x1b\[[0-9;]*m/g, "");
const GREEN = "\x1b[32m";
afterEach(async () => {
  await Promise.all(stores.splice(0).map((s) => s.stop()));
});

function setup(initial = [item("Call dentist", { priority: Priority.MEDIUM }), item("Buy milk"), item("Renew passport")]) {
  const daemon = fakeDaemon(initial);
  const store = daemon.store();
  stores.push(store);
  store.start();
  const ui = render(
    <LiveStoreProvider store={store}>
      <App />
    </LiveStoreProvider>,
  );
  const frame = () => stripAnsi(ui.lastFrame() ?? "");
  const selectedLine = () => frame().split("\n").find((l) => l.includes("›"));
  return { daemon, store, ui, frame, selectedLine };
}

const press = async (ui: ReturnType<typeof render>, key: string) => {
  ui.stdin.write(key);
  await new Promise((r) => setTimeout(r, 20));
};

describe("Inbox", () => {
  it("renders rows with glyph, priority and title, plus the status bar", async () => {
    const { frame } = setup();
    await vi.waitFor(() => expect(frame()).toContain("● live"));
    expect(frame()).toMatch(/› ○ \+3\s+Call dentist/);
    expect(frame()).toContain("Buy milk");
    expect(frame()).toContain("Renew passport");
    expect(frame()).toContain("rev 3");
  });

  it("only shows inbox items", async () => {
    const { daemon, frame } = setup([
      item("Inboxed"),
      item("Doing", { status: Status.IN_PROGRESS }),
      item("Finished", { status: Status.DONE }),
    ]);
    await vi.waitFor(() => expect(frame()).toContain("● live"));
    expect(frame()).toContain("Inboxed");
    expect(frame()).not.toContain("Doing");
    expect(frame()).not.toContain("Finished");
    // The query itself selects the inbox; the fake answers it like the daemon.
    const req = daemon.requests.at(-1);
    expect(req?.filter?.allOf).toHaveLength(1);
    expect(req?.sort.map((k) => k.key)).toEqual([ItemField.CREATED_AT, ItemField.ID]);
  });

  it("moves the selection with j and k, clamped at the ends", async () => {
    const { ui, frame, selectedLine } = setup();
    await vi.waitFor(() => expect(frame()).toContain("● live"));
    await press(ui, "j");
    expect(selectedLine()).toContain("Buy milk");
    await press(ui, "j");
    await press(ui, "j");
    expect(selectedLine()).toContain("Renew passport");
    await press(ui, "k");
    expect(selectedLine()).toContain("Buy milk");
  });

  it("adds a pushed change live without moving the selection", async () => {
    const { daemon, ui, frame, selectedLine } = setup();
    await vi.waitFor(() => expect(frame()).toContain("● live"));
    await press(ui, "j");
    expect(selectedLine()).toContain("Buy milk");

    // A capture with an earlier reference time sorts first, so the new row
    // lands above the selection.
    const early = item("Early bird", { createdAt: new Date(Date.UTC(2025, 0, 1)) });
    daemon.put(early);
    await vi.waitFor(() => expect(frame()).toContain("Early bird"));
    expect(frame()).toContain("rev 4");
    expect(selectedLine()).toContain("Buy milk");
    expect(frame().indexOf("Early bird")).toBeLessThan(frame().indexOf("Call dentist"));
  });

  it("removes an item that leaves the inbox and keeps a nearby selection", async () => {
    const [a, b, c] = [item("A"), item("B"), item("C")];
    const { daemon, ui, frame, selectedLine } = setup([a, b, c]);
    await vi.waitFor(() => expect(frame()).toContain("● live"));
    await press(ui, "j");
    daemon.put({ ...b, status: Status.NEXT });
    await vi.waitFor(() => expect(frame()).not.toMatch(/○\s+B$/m));
    expect(selectedLine()).toMatch(/C$/);
  });

  it("truncates long titles to one line", async () => {
    const long = "word ".repeat(60).trim();
    const { frame } = setup([item(long)]);
    await vi.waitFor(() => expect(frame()).toContain("● live"));
    const lines = frame().split("\n");
    expect(lines.filter((l) => l.includes("word"))).toHaveLength(1);
    expect(frame()).toContain("…");
  });

  it("highlights newly appeared rows briefly", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"], shouldAdvanceTime: true });
    try {
      const { daemon, ui, frame } = setup([item("Old")]);
      await vi.waitFor(() => expect(frame()).toContain("● live"));
      const line = (title: string) => ui.lastFrame()!.split("\n").find((l) => l.includes(title));
      expect(line("Old")).not.toContain(GREEN);
      daemon.put(item("New"));
      await vi.waitFor(() => expect(frame()).toContain("New"));
      expect(line("New")).toContain(GREEN);
      vi.advanceTimersByTime(2000);
      await vi.waitFor(() => expect(line("New")).not.toContain(GREEN));
    } finally {
      vi.useRealTimers();
    }
  });

  it("shows reconnecting, then resumes without losing changes", async () => {
    const { daemon, frame } = setup([item("A")]);
    await vi.waitFor(() => expect(frame()).toContain("● live"));
    daemon.dropStreams();
    daemon.put(item("Missed while down"));
    await vi.waitFor(() => expect(frame()).toContain("Missed while down"));
    await vi.waitFor(() => expect(frame()).toContain("● live"));
    expect(frame()).toContain("rev 2");
  });

  it("quits on q", async () => {
    const { ui, frame } = setup();
    await vi.waitFor(() => expect(frame()).toContain("● live"));
    await press(ui, "q");
    const after = ui.frames.length;
    await press(ui, "j");
    expect(frame().trim()).toBe("");
    expect(ui.frames.length).toBe(after);
  });
});
