import { ItemField, type ListItemsRequest, Status, timestampFromDate } from "@todo-tui/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, item } from "../fake.test-util.js";
import { searchTab } from "../state/tabs.js";
import { setup, stopControllers } from "./setup.test-util.js";

afterEach(async () => {
  await stopControllers();
  vi.useRealTimers();
});

/** The MATCHES text a ListItems narrows by (`/`'s, inside a :search's group too), if any. */
function narrowedBy(req: ListItemsRequest): string | undefined {
  const clauses = [...(req.filter?.allOf ?? []).flatMap((c) => (c.kind.case === "group" ? c.kind.value.anyOf : [])), ...(req.filter?.anyOf ?? [])];
  for (const c of clauses) {
    if (c.kind.case === "predicate" && c.kind.value.target === ItemField.TITLE && c.kind.value.operands[0]?.kind.case === "text") {
      if (req.viewId !== undefined || req.filter?.allOf.length) return c.kind.value.operands[0].kind.value;
    }
  }
  return undefined;
}

const groceries = () =>
  fakeDaemon([item("bread"), item("milk"), item("eggs"), item("milkshake", { labels: ["treat"] }), item("oat milk"), item("butter")]);

describe("/ narrowing", () => {
  it("narrows on the server as you type, one request in flight; enter keeps it; the badge stays (hard case 49)", async () => {
    const t = await setup(groceries());
    const inbox = t.state().tab ?? "";
    const before = t.requests.length;
    await t.c.press("/");
    expect(t.mode().kind).toBe("search");
    // One keystroke at a time, in one tick: the first is sent, the rest coalesce to the latest.
    for (const ch of "milkshake") t.c.input(ch, {});
    await t.c.previewIdle();
    const narrowed = t.requests.slice(before).map(narrowedBy).filter((x) => x !== undefined);
    expect(narrowed).toEqual(["m", "milkshake"]);
    expect(t.requests.slice(before).every((r) => narrowedBy(r) === undefined || r.viewId === inbox)).toBe(true);
    expect(t.titles()).toEqual(["milkshake"]);
    expect(t.selected()).toBe("milkshake");
    // The tab's own count is untouched.
    expect(t.store.getCounts().views.get(inbox)).toBe(6);
    await t.c.press("<enter>");
    expect(t.mode().kind).toBe("normal");
    expect(t.state().narrow).toMatchObject({ shown: "milkshake" });
    expect(t.titles()).toEqual(["milkshake"]);
    // The labels match too.
    await t.c.press("/<ctrl+u>treat<enter>");
    expect(t.titles()).toEqual(["milkshake"]);
  });

  it("lets no LiveView of a prefix outlive its keystroke (hard case 49)", async () => {
    const t = await setup(groceries());
    await t.c.press("/");
    for (const ch of "milk") t.c.input(ch, {});
    await t.c.previewIdle();
    await t.c.press("<backspace>");
    await t.c.previewIdle();
    expect(t.titles()).toEqual(["milk", "milkshake", "oat milk"]);
    // Grace 0: the views of "m" and "milk" are gone a tick after they were let go.
    await new Promise((r) => setTimeout(r, 20));
    const before = t.requests.length;
    t.put(item("milk powder"));
    await vi.waitFor(() => expect(t.titles()).toContain("milk powder"));
    await new Promise((r) => setTimeout(r, 30));
    const requeried = t.requests.slice(before).map((r) => narrowedBy(r) ?? "(own)");
    expect(new Set(requeried)).toEqual(new Set(["(own)", "mil"]));
  });

  it("keeps the selection on its Item while it matches, else goes to the first row; esc goes back to the tab's own selection (hard case 49)", async () => {
    const t = await setup(groceries());
    await t.c.press("jjj");
    expect(t.selected()).toBe("milkshake");
    await t.c.press("/milk");
    await t.c.previewIdle();
    expect(t.titles()).toEqual(["milk", "milkshake", "oat milk"]);
    expect(t.selected()).toBe("milkshake");
    await t.c.press("<ctrl+u>oat");
    await t.c.previewIdle();
    expect(t.selected()).toBe("oat milk");
    await t.c.press("<esc>");
    expect(t.state().narrow).toBeUndefined();
    expect(t.titles()).toHaveLength(6);
    expect(t.selected()).toBe("milkshake");
  });

  it("esc in normal mode clears a kept narrowing, the selection staying on its Item; a tab switch clears it too", async () => {
    const t = await setup(groceries());
    await t.c.press("/milk<enter>j");
    expect(t.selected()).toBe("milkshake");
    await t.c.press("<esc>");
    expect(t.state().narrow).toBeUndefined();
    expect(t.titles()).toHaveLength(6);
    expect(t.selected()).toBe("milkshake");
    await t.c.press("/milk<enter>2");
    expect(t.state().narrow).toBeUndefined();
    await t.c.press("1");
    expect(t.titles()).toHaveLength(6);
  });

  it("narrows nothing for blank or token-less text, and caps the text at 1024 bytes (hard case 50)", async () => {
    const t = await setup(groceries());
    const before = t.requests.length;
    await t.c.press("/   -- ");
    await t.c.previewIdle();
    expect(t.titles()).toHaveLength(6);
    expect(t.requests.slice(before).map(narrowedBy).filter((x) => x !== undefined)).toEqual([]);
    await t.c.press("<enter>");
    expect(t.state().narrow).toBeUndefined();
    // A long paste stops at the limit; the request is valid, and matches nothing.
    await t.c.press("/");
    t.c.paste("é".repeat(600));
    await t.c.previewIdle();
    const line = t.mode().kind === "search" ? (t.mode() as { line: { text: string } }).line.text : "";
    expect(Buffer.byteLength(line, "utf8")).toBe(1024);
    expect(t.store.view({ viewId: t.state().tab ?? "" }).getSnapshot().status).toBe("ready");
    expect(t.titles()).toEqual([]);
    expect(t.state().toast).toBeUndefined();
  });

  it("keeps the marks while narrowing, hidden ones too: a bulk action takes them all (hard case 24)", async () => {
    const t = await setup(groceries());
    await t.c.press("mm");
    await t.c.press("/milk");
    await t.c.previewIdle();
    await t.c.press("<enter>");
    // bread is hidden, milk shows.
    expect(t.state().marks.size).toBe(2);
    expect(t.c.currentIds().filter((id) => t.state().marks.has(id))).toHaveLength(1);
    // esc clears the narrowing first, then the marks.
    await t.c.press("<esc>");
    expect(t.state().narrow).toBeUndefined();
    expect(t.state().marks.size).toBe(2);
    await t.c.press("/milk<enter>x");
    expect(t.toast()).toBe("mark done (2 items) · u undo");
    expect(t.byTitle("bread")?.status).toBe(Status.DONE);
    expect(t.byTitle("milk")?.status).toBe(Status.DONE);
  });
});

describe(":search", () => {
  const NOW = new Date(Date.UTC(2026, 9, 26, 12));

  const everything = () =>
    fakeDaemon(
      [
        item("milk run", { updatedAt: timestampFromDate(new Date(Date.UTC(2026, 9, 1))) }),
        item("old milk", { status: Status.DONE, updatedAt: timestampFromDate(new Date(Date.UTC(2026, 9, 2))) }),
        item("later milk", { deferUntil: { year: 2026, month: 12, day: 1 }, updatedAt: timestampFromDate(new Date(Date.UTC(2026, 9, 3))) }),
        item("cheese", { description: "with some milk", updatedAt: timestampFromDate(new Date(Date.UTC(2026, 9, 4))) }),
        item("bread"),
      ],
      { now: () => NOW, timeZone: "UTC" },
    );

  it("opens a Search tab after the views, over done and deferred Items and descriptions, newest change first", async () => {
    const t = await setup(everything(), { now: () => NOW });
    expect(t.titles()).toEqual(["milk run", "cheese", "bread"]);
    await t.c.press(":search milk<enter>");
    expect(t.state().tab).toBe(searchTab("milk"));
    expect(t.state().search).toMatchObject({ query: "milk", from: t.viewId("inbox") });
    await vi.waitFor(() => expect(t.titles()).toEqual(["cheese", "later milk", "old milk", "milk run"]));
    // The deferred Item, which no seeded view shows, is reachable: x acts on it.
    await t.c.press("jx");
    expect(t.byTitle("later milk")?.status).toBe(Status.DONE);
    // / narrows it further.
    await t.c.press("/run<enter>");
    await vi.waitFor(() => expect(t.titles()).toEqual(["milk run"]));
  });

  it("is replaced by a new search, closed by leaving it, and esc goes back where it came from", async () => {
    const t = await setup(everything(), { now: () => NOW });
    await t.c.press("2:search milk<enter>");
    await t.c.press(":search cheese<enter>");
    expect(t.state().tab).toBe(searchTab("cheese"));
    await vi.waitFor(() => expect(t.titles()).toEqual(["cheese"]));
    // It came from Today, through the first search.
    await t.c.press("<esc>");
    expect(t.tabIs("today")).toBe(true);
    expect(t.state().search).toBeUndefined();
    // ] from the last view goes to it while open, and on from it wraps, closing it.
    await t.c.press(":search milk<enter>]");
    expect(t.tabIs("inbox")).toBe(true);
    expect(t.state().search).toBeUndefined();
    await t.c.press(":search milk<enter>1");
    expect(t.state().search).toBeUndefined();
    await t.c.press("6:search milk<enter>[");
    expect(t.tabIs("archive")).toBe(true);
  });

  it("asks for words when there are none", async () => {
    const t = await setup(everything(), { now: () => NOW });
    await t.c.press(":search -- <enter>");
    expect(t.toast()).toBe("Search for what? :search <words>");
    expect(t.tabIs("inbox")).toBe(true);
  });
});
