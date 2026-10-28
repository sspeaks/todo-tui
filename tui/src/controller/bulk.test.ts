import { type ItemsClient, Priority, Status, archiveItem, deleteItem, updateItem } from "@todo-tui/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, item, rows } from "../fake.test-util.js";
import { holding, setup, stopControllers } from "./setup.test-util.js";

afterEach(async () => {
  await stopControllers();
  vi.useRealTimers();
});

interface Call {
  readonly method: string;
  readonly req: { requestId?: string; mutations?: unknown[] };
}

/** An ItemsClient that logs every call it passes on. */
const recording =
  (log: Call[]) =>
  (items: ItemsClient): ItemsClient =>
    new Proxy(items, {
      get(target, prop, receiver) {
        const value = Reflect.get(target, prop, receiver);
        if (typeof prop !== "string" || typeof value !== "function") return value;
        return (req: Call["req"], opts: unknown) => {
          log.push({ method: prop, req });
          return (value as (r: unknown, o: unknown) => unknown).call(target, req, opts);
        };
      },
    });

const batches = (log: Call[]) => log.filter((c) => c.method === "batchMutate");
const marked = (t: Awaited<ReturnType<typeof setup>>) => [...t.state().marks].map((id) => t.store.getItem(id)?.title).sort();

describe("marks", () => {
  it("m toggles a mark and moves down", async () => {
    const t = await setup(fakeDaemon(rows(5)));
    await t.c.press("mjm");
    expect(marked(t)).toEqual(["row 0", "row 2"]);
    expect(t.selected()).toBe("row 3");
    await t.c.press("km");
    expect(marked(t)).toEqual(["row 0"]);
    expect(t.selected()).toBe("row 3");
    // On the last row it stays there.
    await t.c.press("Gm");
    expect(t.selected()).toBe("row 4");
    expect(marked(t)).toEqual(["row 0", "row 4"]);
  });

  it("V marks the rows between where it started and the selection as it moves; V or esc ends it, keeping the marks", async () => {
    const t = await setup(fakeDaemon(rows(6)));
    await t.c.press("mV");
    expect(t.state().visual).toBeDefined();
    expect(marked(t)).toEqual(["row 0", "row 1"]);
    await t.c.press("jj");
    expect(marked(t)).toEqual(["row 0", "row 1", "row 2", "row 3"]);
    // Back past the anchor: the range shrinks, and the marks from before V stay.
    await t.c.press("kkk");
    expect(t.selected()).toBe("row 0");
    expect(marked(t)).toEqual(["row 0", "row 1"]);
    await t.c.press("G");
    expect(marked(t)).toEqual(["row 0", "row 1", "row 2", "row 3", "row 4", "row 5"]);
    await t.c.press("kV");
    expect(t.state().visual).toBeUndefined();
    expect(marked(t)).toEqual(["row 0", "row 1", "row 2", "row 3", "row 4"]);
    // Moving no longer marks; esc ends a range first, then clears the marks.
    await t.c.press("kVj<esc>");
    expect(t.state().visual).toBeUndefined();
    expect(marked(t)).toEqual(["row 0", "row 1", "row 2", "row 3", "row 4"]);
    await t.c.press("<esc>");
    expect(t.state().marks.size).toBe(0);
  });

  it("belong to the tab: a tab switch clears them", async () => {
    const t = await setup(fakeDaemon(rows(3)));
    await t.c.press("mmV");
    expect(t.state().marks.size).toBe(3);
    await t.c.press("2");
    expect(t.state().marks.size).toBe(0);
    expect(t.state().visual).toBeUndefined();
    await t.c.press("1");
    expect(t.state().marks.size).toBe(0);
  });

  it("are pruned when the tab loses the Item, and kept across a reorder (hard cases 22, 24)", async () => {
    const fd = fakeDaemon([
      item("a", { status: Status.NEXT, priority: Priority.LOW }),
      item("b", { status: Status.NEXT, priority: Priority.LOW }),
      item("c", { status: Status.NEXT, priority: Priority.LOW }),
    ]);
    const t = await setup(fd);
    await t.c.press("3");
    await vi.waitFor(() => expect(t.titles()).toEqual(["a", "b", "c"]));
    await t.c.press("mmm");
    // Another client moves c to the top: the marks follow the Items.
    const other = t.clients().items;
    await updateItem(other, { id: t.byTitle("c")!.id, priority: Priority.HIGHEST }, ["priority"]);
    await vi.waitFor(() => expect(t.titles()).toEqual(["c", "a", "b"]));
    expect(marked(t)).toEqual(["a", "b", "c"]);
    // And deletes b: its mark goes with it.
    await deleteItem(other, t.byTitle("b")!.id);
    await vi.waitFor(() => expect(marked(t)).toEqual(["a", "c"]));
    await t.c.press("x");
    expect(t.toast()).toBe("mark done (2 items) · u undo");
    expect(t.byTitle("a")?.status).toBe(Status.DONE);
    expect(t.byTitle("c")?.status).toBe(Status.DONE);
  });
});

describe("bulk actions", () => {
  it("x with 3 marks: one BatchMutate in list order, one undo entry, marks cleared, the selection at the first marked index (hard case 21)", async () => {
    const log: Call[] = [];
    const t = await setup(fakeDaemon(rows(5)), { items: recording(log) });
    // Marked out of list order: the batch still goes in list order.
    await t.c.press("jjjmkkkm");
    await t.c.press("m");
    expect(marked(t)).toEqual(["row 1", "row 2", "row 3"]);
    await t.c.press("x");
    expect(batches(log)).toHaveLength(1);
    const ids = (batches(log)[0]?.req.mutations as { kind: { value: { id: string } } }[]).map((m) => t.store.getItem(m.kind.value.id)?.title);
    expect(ids).toEqual(["row 1", "row 2", "row 3"]);
    expect(t.userOps().map((o) => o.summary)).toEqual(["mark done (3 items)"]);
    expect(t.toast()).toBe("mark done (3 items) · u undo");
    expect(t.titles()).toEqual(["row 0", "row 4"]);
    expect(t.state().marks.size).toBe(0);
    expect(t.list().anchor).toBe(1);
    expect(t.selected()).toBe("row 4");
    expect(log.some((c) => c.method === "setStatus")).toBe(false);
  });

  it("u after a bulk x brings all of them back, the first selected (hard case 26)", async () => {
    const t = await setup(fakeDaemon(rows(5)));
    await t.c.press("jmmmx");
    expect(t.titles()).toEqual(["row 0", "row 4"]);
    await t.c.press("u");
    expect(t.toast()).toBe("Undid: mark done (3 items)");
    expect(t.titles()).toEqual(["row 0", "row 1", "row 2", "row 3", "row 4"]);
    expect(t.selected()).toBe("row 1");
  });

  it("acts on the marks still in the tab: one deleted elsewhere before x is left out (hard case 22)", async () => {
    const log: Call[] = [];
    const t = await setup(fakeDaemon(rows(4)), { items: recording(log) });
    await t.c.press("mmm");
    await deleteItem(t.clients().items, t.byTitle("row 1")!.id);
    await vi.waitFor(() => expect(marked(t)).toEqual(["row 0", "row 2"]));
    await t.c.press("x");
    expect(batches(log)).toHaveLength(1);
    expect(batches(log)[0]?.req.mutations).toHaveLength(2);
    expect(t.toast()).toBe("mark done (2 items) · u undo");
  });

  it("drops an Item deleted during the batch and retries once with a new request_id (hard case 23)", async () => {
    const log: Call[] = [];
    const h = holding(["batchMutate"]);
    const t = await setup(fakeDaemon(rows(4)), { items: (items) => h.wrap(recording(log)(items)) });
    await t.c.press("mmm");
    h.hold();
    t.c.input("x", {});
    await vi.waitFor(() => expect(h.held.count).toBe(1));
    await deleteItem(t.clients().items, t.byTitle("row 1")!.id);
    h.release();
    await t.c.idle();
    const [first, retry] = batches(log);
    expect(batches(log)).toHaveLength(2);
    expect(first?.req.mutations).toHaveLength(3);
    expect(retry?.req.mutations).toHaveLength(2);
    expect(retry?.req.requestId).not.toBe(first?.req.requestId);
    expect(t.toast()).toBe("mark done (2 items) · u undo · 1 was deleted elsewhere");
    expect(t.byTitle("row 0")?.status).toBe(Status.DONE);
    expect(t.byTitle("row 2")?.status).toBe(Status.DONE);
    expect(t.userOps().map((o) => o.summary)).toEqual(['delete "row 1"', "mark done (2 items)"]);
  });

  it("refuses more than 1000 marks, sending nothing (hard case 25)", async () => {
    const log: Call[] = [];
    const t = await setup(fakeDaemon(rows(1500)), { items: recording(log) });
    expect(t.titles()).toHaveLength(1500);
    await t.c.press("VG");
    expect(t.state().marks.size).toBe(1500);
    for (const key of ["x", "p1", "t", "dd", "s", "z"]) {
      await t.c.press(key);
      expect(t.toast(), key).toBe("Bulk actions take at most 1000 items");
      expect(t.state().toast?.level).toBe("error");
      expect(t.mode().kind).toBe("normal");
    }
    expect(log.filter((c) => c.method !== "getUndoState" && c.method !== "listItems")).toEqual([]);
    expect(t.userOps()).toEqual([]);
    // Unmarking down to 1000 lets it through, in one batch.
    await t.c.press("<esc><esc>ggVG");
    for (let i = 0; i < 499; i++) t.c.input("k", {});
    await t.c.idle();
    expect(t.state().marks.size).toBe(1001);
    await t.c.press("k");
    expect(t.state().marks.size).toBe(1000);
    await t.c.press("x");
    expect(batches(log)).toHaveLength(1);
    expect(t.toast()).toBe("mark done (1000 items) · u undo");
  });

  it("p, s, W and z act on every marked Item in one op (hard case 5)", async () => {
    const t = await setup(
      fakeDaemon([
        item("a", { waiting: { on: { ref: { case: "freeText", value: "dana" } } } }),
        item("b", { waiting: { on: { ref: { case: "freeText", value: "eve" } } } }),
        item("c"),
      ]),
    );
    await t.c.press("mmmp3");
    expect(t.userOps().map((o) => o.summary)).toEqual(["edit 3 items"]);
    expect(t.toast()).toBe("edit 3 items · u undo");
    expect(["a", "b", "c"].map((x) => t.byTitle(x)?.priority)).toEqual([Priority.MEDIUM, Priority.MEDIUM, Priority.MEDIUM]);
    expect(t.state().marks.size).toBe(0);
    // W clears waiting on the two that wait; c is left out.
    await t.c.press("ggmmmW");
    expect(t.userOps().at(-1)?.summary).toBe("edit 2 items");
    expect(t.byTitle("a")?.waiting).toBeUndefined();
    // s, with the status they share ticked, moves both.
    await t.c.press("ggmm");
    await t.c.press("s");
    expect(t.mode()).toMatchObject({ kind: "picker", index: 0, target: { bulk: true } });
    await t.c.press("n");
    expect(t.toast()).toBe("move to next (2 items) · u undo");
    expect(t.titles()).toEqual(["c"]);
  });

  it("x on marks already done says so and sends nothing", async () => {
    const t = await setup(fakeDaemon([item("p", { status: Status.DONE }), item("q", { status: Status.DONE })]));
    await t.c.press("6");
    await vi.waitFor(() => expect(t.titles()).toHaveLength(2));
    await t.c.press("ggmmx");
    expect(t.toast()).toBe("all already done");
    expect(t.userOps()).toEqual([]);
    expect(t.state().marks.size).toBe(2);
  });

  it("z on a mixed set archives the unarchived ones; on an all-archived set, unarchives them", async () => {
    const t = await setup(fakeDaemon([item("p", { status: Status.DONE }), item("q", { status: Status.DONE })]));
    await archiveItem(t.clients().items, t.byTitle("q")!.id);
    await t.c.press("6");
    await vi.waitFor(() => expect(t.titles()).toHaveLength(2));
    await t.c.press("ggmmz");
    expect(t.userOps().at(-1)?.summary).toBe('archive "p"');
    expect(t.toast()).toBe('archive "p" · u undo');
    expect(t.byTitle("p")?.archived).toBeDefined();
    await t.c.press("ggmmz");
    expect(t.userOps().at(-1)?.summary).toBe("unarchive 2 items");
    expect(t.byTitle("p")?.archived).toBeUndefined();
    expect(t.byTitle("q")?.archived).toBeUndefined();
  });

  it("dd with marks asks “Delete N items?” and deletes them in one op, a marked subtask with its marked parent", async () => {
    const parent = item("parent");
    const t = await setup(fakeDaemon([parent, item("child", { parentId: parent.id }), item("other"), item("kept")]));
    await t.c.press("mmm");
    await t.c.press("dd");
    expect(t.mode()).toMatchObject({ kind: "confirm", text: "Delete 3 items? y/N" });
    // Any other key cancels.
    await t.c.press("n");
    expect(t.userOps()).toEqual([]);
    expect(t.state().marks.size).toBe(3);
    await t.c.press("ddy");
    expect(t.userOps().map((o) => o.summary)).toEqual(["delete 2 items and 1 subtask"]);
    expect(t.titles()).toEqual(["kept"]);
    expect(t.byTitle("child")?.deletedAt).toBeDefined();
    expect(t.toast()).toBe("delete 2 items and 1 subtask · u undo");
  });

  it("dd with marks counts the subtasks that go with them in the confirm, those not marked themselves", async () => {
    const a = item("a");
    const b = item("b");
    const t = await setup(
      fakeDaemon([a, b, item("a.1", { parentId: a.id }), item("a.2", { parentId: a.id, status: Status.DONE }), item("b.1", { parentId: b.id }), item("c")]),
    );
    expect(t.titles()).toEqual(["a", "b", "a.1", "b.1", "c"]);
    // a and b marked, and b.1: a's two subtasks (one done) go with it; b.1 is counted as marked.
    await t.c.press("mmjm");
    await t.c.press("dd");
    expect(t.mode()).toMatchObject({ kind: "confirm", text: "Delete 3 items and their 2 subtasks? y/N" });
    await t.c.press("y");
    expect(t.titles()).toEqual(["c"]);
    expect(t.byTitle("a.2")?.deletedAt).toBeDefined();
  });

  it("t with marks takes +a -b, applied to each Item's own labels; unchanged ones are left out", async () => {
    const t = await setup(fakeDaemon([item("a", { labels: ["later", "home"] }), item("b", { labels: ["urgent"] }), item("c")]));
    await t.c.press("mmmt");
    expect(t.mode()).toMatchObject({ kind: "prompt", prompt: "labels", line: { text: "" }, target: { bulk: true } });
    await t.c.press("+urgent -later<enter>");
    expect(t.mode().kind).toBe("normal");
    expect(t.byTitle("a")?.labels).toEqual(["home", "urgent"]);
    expect(t.byTitle("b")?.labels).toEqual(["urgent"]);
    expect(t.byTitle("c")?.labels).toEqual(["urgent"]);
    expect(t.userOps().map((o) => o.summary)).toEqual(["edit 2 items"]);
    expect(t.toast()).toBe("edit 2 items · u undo");
    // Nothing to change: closes, and says so.
    await t.c.press("ggmmmt+urgent<enter>");
    expect(t.toast()).toBe("Labels unchanged");
    expect(t.userOps()).toHaveLength(1);
  });

  it("^ and w with marks: the shared value prefilled, each Item set as one would be", async () => {
    const NOON = new Date(Date.UTC(2026, 9, 26, 12));
    const fd = fakeDaemon(
      [
        item("a", { waiting: { on: { ref: { case: "freeText", value: "dana" } }, since: { year: 2026, month: 10, day: 1 } } }),
        item("b"),
      ],
      { now: () => NOON, timeZone: "UTC" },
    );
    const t = await setup(fd, { now: () => NOON });
    await t.c.press("mm^");
    expect(t.mode()).toMatchObject({ kind: "prompt", prompt: "due", line: { text: "" } });
    await t.c.press("2026-11-02<enter>");
    expect(t.userOps().at(-1)?.summary).toBe("edit 2 items");
    expect(["a", "b"].map((x) => t.byTitle(x)?.dueDate)).toMatchObject([{ day: 2 }, { day: 2 }]);
    await t.c.press("mm^");
    expect(t.mode()).toMatchObject({ line: { text: "2026-11-02" } });
    await t.c.press("<esc>");
    await t.c.press("w");
    expect(t.mode()).toMatchObject({ kind: "prompt", prompt: "waiting", line: { text: "" } });
    await t.c.press("dana ^2026-11-05<enter>");
    // a waits on dana already: only the follow-up changes, and since stays.
    expect(t.byTitle("a")?.waiting).toMatchObject({ since: { day: 1 }, followUpOn: { day: 5 } });
    expect(t.byTitle("b")?.waiting).toMatchObject({ on: { ref: { value: "dana" } }, followUpOn: { day: 5 } });
    expect(t.userOps().at(-1)?.summary).toBe("edit 2 items");
  });

  it("keeps the marks when the batch fails, with the error", async () => {
    const t = await setup(fakeDaemon(rows(3)), {
      items: (items) =>
        new Proxy(items, {
          get: (target, prop, receiver) =>
            prop === "batchMutate"
              ? async () => {
                  throw new Error("boom");
                }
              : Reflect.get(target, prop, receiver),
        }),
    });
    await t.c.press("mmx");
    expect(t.state().toast?.level).toBe("error");
    expect(t.state().marks.size).toBe(2);
  });
});
