import { Code, ConnectError, ErrorInfoSchema, ItemField, Operator, Status } from "@todo-tui/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, item, rows } from "../fake.test-util.js";
import { setup, stopControllers } from "./setup.test-util.js";
import { viewWords } from "./views.js";

afterEach(async () => {
  await stopControllers();
});

const statusIs = (s: Status) => ({ allOf: [{ kind: { case: "predicate" as const, value: { target: ItemField.STATUS, op: Operator.IN, operands: [{ kind: { case: "value" as const, value: { kind: { case: "status" as const, value: s } } } }] } } }] });

/** A daemon with two Items next, and query text for them the fake parses. */
async function withNext() {
  const fd = fakeDaemon([...rows(3), item("n1", { status: Status.NEXT }), item("n2", { status: Status.NEXT })]);
  fd.daemon.defineQuery("is:next", { filter: statusIs(Status.NEXT) });
  fd.daemon.defineQuery("is:next count:off", { filter: statusIs(Status.NEXT), showCount: false });
  return setup(fd);
}

const names = (t: Awaited<ReturnType<typeof setup>>) => t.store.getViews().views.map((v) => v.name);

describe(":view", () => {
  it("splits its words, a quoted name whole", () => {
    expect(viewWords('rename "Deep work" Focus time').words).toEqual(["rename", "Deep work", "Focus", "time"]);
    expect(viewWords("new Work is:next #a").rest(2)).toBe("is:next #a");
  });

  it("new opens the query prompt with a live count, and enter creates the view and goes to it", async () => {
    const t = await withNext();
    await t.c.press(":view new Work<enter>");
    expect(t.mode()).toMatchObject({ kind: "query", name: "Work" });
    await t.c.press("is:next");
    await t.c.previewIdle();
    expect(t.mode()).toMatchObject({ preview: { for: "is:next", text: "→ 2 items", warn: false } });
    await t.c.press("<enter>");
    await vi.waitFor(() => expect(names(t)).toContain("Work"));
    expect(t.mode().kind).toBe("normal");
    expect(t.toast()).toBe("Created view “Work”");
    expect(t.state().tab).toBe(t.store.getViews().views.find((v) => v.name === "Work")?.id);
  });

  it("new with a query creates at once; an error keeps the prompt open with its span", async () => {
    const t = await withNext();
    await t.c.press(":view new Later is:next count:off<enter>");
    await vi.waitFor(() => expect(names(t)).toContain("Later"));
    expect(t.store.getViews().views.find((v) => v.name === "Later")?.showCount).toBe(false);

    await t.c.press(":view new Bad nope<enter>");
    await t.c.previewIdle();
    expect(t.mode()).toMatchObject({ kind: "query", name: "Bad", preview: { warn: true, span: { start: 0, end: 4 } } });
    expect(names(t)).not.toContain("Bad");
    await t.c.press("<esc>");
    expect(t.mode().kind).toBe("normal");
  });

  it("refuses a taken name before opening the prompt", async () => {
    const t = await withNext();
    await t.c.press(":view new inbox<enter>");
    expect(t.mode().kind).toBe("normal");
    expect(t.toast()).toBe("There is a view named “inbox” already");
  });

  it("edit prefills the view's query and saves a change", async () => {
    const t = await withNext();
    await t.c.press(":view edit Next<enter>");
    expect(t.mode()).toMatchObject({ kind: "query", name: "Next", line: { text: "is:next,in_progress sort:-effective_priority,due_date" } });
    await t.c.press("<ctrl+u>is:next<enter>");
    await vi.waitFor(() => expect(t.toast()).toBe("Saved view “Next”"));
    const next = t.store.getViews().views.find((v) => v.name === "Next");
    expect(next?.sort).toEqual([]);
  });

  it("edit with nothing named edits the current tab; unchanged text saves nothing", async () => {
    const t = await withNext();
    const calls = t.daemon.ops.length;
    await t.c.press(":view edit<enter>");
    expect(t.mode()).toMatchObject({ kind: "query", name: "Inbox" });
    await t.c.press("<enter>");
    expect(t.mode().kind).toBe("normal");
    expect(t.daemon.ops.length).toBe(calls);
  });

  it("renames, moves and deletes with a confirm", async () => {
    const t = await withNext();
    await t.c.press(":view rename Someday Later<enter>");
    await vi.waitFor(() => expect(names(t)).toContain("Later"));
    expect(t.toast()).toBe("Renamed view to “Later”");
    await t.c.press(":view rename Later Inbox<enter>");
    await vi.waitFor(() => expect(t.toast()).toBe("Another view has that name"));

    await t.c.press(":view move Later first<enter>");
    await vi.waitFor(() => expect(names(t)[0]).toBe("Later"));
    await t.c.press(":view move Later right<enter>");
    await vi.waitFor(() => expect(names(t)[1]).toBe("Later"));
    await t.c.press(":view move Later 6<enter>");
    await vi.waitFor(() => expect(names(t)[5]).toBe("Later"));

    await t.c.press(":view delete Later<enter>");
    expect(t.mode()).toMatchObject({ kind: "confirm", action: "delete_view", text: "Delete view “Later”? It will not come back y/N" });
    await t.c.press("n");
    expect(names(t)).toContain("Later");
    await t.c.press(":view delete Later<enter>y");
    await vi.waitFor(() => expect(names(t)).not.toContain("Later"));
    expect(t.toast()).toBe("Deleted view “Later”");
  });

  it("deleting the current tab falls back to the first", async () => {
    const t = await withNext();
    await t.c.press("3");
    expect(t.tabIs("next")).toBe(true);
    await t.c.press(":view delete<enter>y");
    await vi.waitFor(() => expect(names(t)).not.toContain("Next"));
    await vi.waitFor(() => expect(t.tabIs("inbox")).toBe(true));
  });

  it("an edit that lost a race shows the view's new query, and enter overwrites it", async () => {
    const t = await withNext();
    await t.c.press(":view edit Next<enter>");
    const next = t.store.getViews().views.find((v) => v.name === "Next")!;
    // Changed elsewhere meanwhile.
    t.daemon.updateView(next.id, { filter: statusIs(Status.SOMEDAY), query: "is:someday" });
    await vi.waitFor(() => expect(t.store.getSavedView(next.id)?.query).toBe("is:someday"));
    t.daemon.defineQuery("is:inbox", { filter: statusIs(Status.INBOX) });
    await t.c.press("<ctrl+u>is:inbox<enter>");
    await vi.waitFor(() => expect(t.mode()).toMatchObject({ kind: "query", conflict: "is:someday" }));
    await t.c.press("<enter>");
    await vi.waitFor(() => expect(t.store.getSavedView(next.id)?.query).toBe("is:inbox"));
  });

  it("a view that keeps failing its revision rebases a few times at most, then shows the conflict", async () => {
    const fd = fakeDaemon(rows(2));
    fd.daemon.defineQuery("is:next", { filter: statusIs(Status.NEXT) });
    const real = fd.clients().views;
    let updates = 0;
    // Every UpdateView loses the race, though the query there is unchanged.
    const views = {
      ...real,
      updateView: async () => {
        updates++;
        throw new ConnectError("stale", Code.Aborted, undefined, [{ desc: ErrorInfoSchema, value: { reason: "REVISION_MISMATCH" } }]);
      },
    } as typeof real;
    const t = await setup(fd, { controller: { views } });
    await t.c.press(":view edit Inbox<enter><ctrl+u>is:next<enter>");
    await vi.waitFor(() => expect(t.mode()).toMatchObject({ kind: "query", conflict: "is:inbox sort:created_at" }));
    expect(updates).toBeLessThanOrEqual(4);
  });

  it("a failure shown on the line goes once the next enter says something else", async () => {
    const t = await withNext();
    await t.c.press(":view rename Next Work<enter>");
    await vi.waitFor(() => expect(names(t)).toContain("Work"));
    await t.c.press(":view edit Work<enter><ctrl+u>is:next");
    // Deleted meanwhile: enter fails, and says so.
    const id = t.store.getViews().views.find((v) => v.name === "Work")!.id;
    t.daemon.removeView(id);
    await t.c.press("<enter>");
    await vi.waitFor(() => expect(t.mode()).toMatchObject({ kind: "query", error: "The view was deleted elsewhere" }));
    // A parse error after it replaces it.
    await t.c.press("<ctrl+u>nope<enter>");
    await vi.waitFor(() => expect(t.mode()).toMatchObject({ kind: "query", error: undefined, preview: { warn: true } }));
  });

  it("says how :view is used", async () => {
    const t = await withNext();
    await t.c.press(":view<enter>");
    expect(t.toast()).toMatch(/^:view new <name> \[query\]/);
    await t.c.press(":view frob<enter>");
    expect(t.toast()).toMatch(/^Not a :view command: frob/);
    await t.c.press(":view edit Nope<enter>");
    expect(t.toast()).toBe("No view named “Nope”");
  });
});
