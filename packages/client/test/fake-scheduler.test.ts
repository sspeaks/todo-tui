// FakeDaemon's scheduler hook (design 3.7): the auto_done archive and its
// healing, as crates/todo-store/tests/store.rs tests auto_archive_tx, and
// the next-instance cases that need state set up behind the API. The
// daemon's own scheduler is tested in crates/todo-daemon/tests/server.rs;
// these cases are the fake's alone, since a spawned daemon's threshold is
// fixed at 7 days. Time moves with the fake's `now` option.
import { create } from "@bufbuild/protobuf";
import { timestampFromMs } from "@bufbuild/protobuf/wkt";
import { createClient } from "@connectrpc/connect";
import { describe, expect, it } from "vitest";
import { ArchiveReason, CreatedVia, ItemSchema, Status, StatusCategory } from "../src/gen/todo/v1/item_pb.js";
import { ItemService } from "../src/gen/todo/v1/item_service_pb.js";
import { OpKind } from "../src/gen/todo/v1/op_pb.js";
import { ARCHIVE_DONE_AFTER_MS, FakeDaemon } from "../src/fake-daemon.js";
import { archiveItem, getUndoState, redo, setStatus, unarchiveItem, undo } from "../src/mutations.js";
import { ulid } from "../src/ulid.js";

const DAY = 24 * 60 * 60 * 1000;
const WEEK = ARCHIVE_DONE_AFTER_MS;

function setup() {
  let now = Date.UTC(2026, 9, 26, 12);
  const daemon = new FakeDaemon([], { now: () => new Date(now), timeZone: "UTC" });
  const items = createClient(ItemService, daemon.transport);
  const capture = async (text: string) => (await items.captureItem({ requestId: ulid(), text, createdVia: CreatedVia.TUI })).item!;
  /** Moves the clock to `ms` after the start. */
  const start = now;
  const at = (ms: number) => {
    now = start + ms;
  };
  const archived = (id: string) => daemon.items.get(id)?.archived;
  return { daemon, items, capture, at, archived };
}

describe("FakeDaemon runScheduler", () => {
  it("archives done and cancelled Items closed at least the threshold ago, oldest first, as a system op", async () => {
    const { daemon, items, capture, at, archived } = setup();
    const [a, b, open] = [await capture("a"), await capture("b"), await capture("open")];
    await setStatus(items, b.id, Status.CANCELLED);
    at(DAY);
    await setStatus(items, a.id, Status.DONE);
    await setStatus(items, open.id, Status.NEXT);
    const top = (await getUndoState(items)).undo;

    // A week after b's close, before a's, only b is due.
    at(WEEK + 1);
    const first = daemon.runScheduler();
    expect([first.archived, first.healed]).toEqual([[b.id], []]);
    expect(first.ops).toHaveLength(1);
    const op = first.ops[0]!;
    expect([op.kind, op.actor, op.undoState, op.summary]).toEqual([OpKind.AUTO_ARCHIVE, "system", undefined, "auto-archive 1 done item"]);
    expect(op.events).toEqual([{ kind: "set", itemId: b.id, field: "archived", old: undefined, new: archived(b.id) }]);
    // Archived now, which bumps updated_at, so it sorts to the top of Archive.
    expect(archived(b.id)?.reason).toBe(ArchiveReason.AUTO_DONE);
    expect(daemon.items.get(b.id)?.updatedAt).toEqual(archived(b.id)?.at);

    // Nothing more is due: no op, no revision.
    const rev = daemon.revision;
    expect(daemon.runScheduler()).toEqual({ archived: [], healed: [], ops: [] });
    expect(daemon.revision).toBe(rev);

    at(DAY + WEEK - 1);
    expect(daemon.runScheduler().ops).toEqual([]);
    at(DAY + WEEK);
    expect(daemon.runScheduler().archived).toEqual([a.id]);
    expect(archived(open.id)).toBeUndefined();
    // The sweep is no undo entry.
    expect((await getUndoState(items)).undo).toEqual(top);
  });

  it("takes any threshold, and an Item put closed is closed at its updated_at", async () => {
    const { daemon, capture } = setup();
    const x = await capture("x");
    const old = daemon.put({ title: "old", status: Status.DONE, updatedAt: { seconds: 1n } });
    expect(daemon.runScheduler(DAY * 365 * 50).archived).toEqual([old.id]);
    expect(daemon.runScheduler(0).archived).toEqual([]);
    daemon.update(x.id, { status: Status.DONE });
    expect(daemon.runScheduler(0).archived).toEqual([x.id]);
  });

  it("closes an Item with no recorded close time at its updated_at", () => {
    const { daemon } = setup();
    // A restored snapshot carries no close times, as a row written before
    // status_entered_at was.
    const closedAt = Date.UTC(2026, 9, 20, 12);
    const id = ulid();
    daemon.restore(daemon.revision, [create(ItemSchema, { id, title: "old", status: Status.DONE, statusCategory: StatusCategory.DONE, updatedAt: timestampFromMs(closedAt) })]);
    const since = Date.UTC(2026, 9, 26, 12) - closedAt;
    expect(daemon.runScheduler(since + 1).archived).toEqual([]);
    expect(daemon.runScheduler(since).archived).toEqual([id]);
  });

  it("leaves a z after closing alone on day 8 and later, until the Item closes again", async () => {
    const { daemon, items, capture, at, archived } = setup();
    const x = await capture("x");
    await setStatus(items, x.id, Status.DONE);
    at(WEEK + 1);
    expect(daemon.runScheduler().archived).toEqual([x.id]);

    // `z` brings it back out of Archive for good.
    await unarchiveItem(items, x.id);
    for (const days of [8, 30]) {
      at(days * DAY);
      expect(daemon.runScheduler().ops, `day ${days}`).toEqual([]);
    }
    expect(archived(x.id)).toBeUndefined();

    // Closing it again restarts the clock, and the old `z` no longer counts.
    await setStatus(items, x.id, Status.NEXT);
    at(31 * DAY);
    await setStatus(items, x.id, Status.DONE);
    at(31 * DAY + WEEK - 1);
    expect(daemon.runScheduler().ops).toEqual([]);
    at(31 * DAY + WEEK);
    expect(daemon.runScheduler().archived).toEqual([x.id]);
  });

  it("leaves an archive and unarchive by hand alone, but not one that was undone", async () => {
    const { daemon, items, capture, at } = setup();
    const [x, y] = [await capture("x"), await capture("y")];
    await setStatus(items, x.id, Status.DONE);
    await setStatus(items, y.id, Status.DONE);
    await archiveItem(items, x.id);
    await unarchiveItem(items, x.id);
    // A `z` on y by mistake, undone at once: no decision is left in effect.
    await archiveItem(items, y.id);
    await undo(items);
    at(2 * WEEK);
    expect(daemon.runScheduler().archived).toEqual([y.id]);
  });

  it("restarts the clock when a done Item is cancelled", async () => {
    const { daemon, items, capture, at } = setup();
    const [x, y] = [await capture("x"), await capture("y")];
    await setStatus(items, x.id, Status.DONE);
    await setStatus(items, y.id, Status.DONE);
    await archiveItem(items, y.id);
    await unarchiveItem(items, y.id);
    at(DAY);
    await setStatus(items, x.id, Status.CANCELLED);
    await setStatus(items, y.id, Status.CANCELLED);
    // A week after the first close neither is due.
    at(WEEK);
    expect(daemon.runScheduler().ops).toEqual([]);
    // A week after the cancel both are: y's unarchive came before it.
    at(DAY + WEEK + 1);
    expect(daemon.runScheduler().archived).toEqual([x.id, y.id]);
  });

  it("heals an Item an undo reopened, keeping the redo stack, and archives it again after a redo", async () => {
    const { daemon, items, capture, at, archived } = setup();
    const x = await capture("x");
    const done = await setStatus(items, x.id, Status.DONE);
    at(WEEK + 1);
    expect(daemon.runScheduler().archived).toEqual([x.id]);

    // Undo reverts the completion (the sweep is no entry), which reopens x
    // but keeps the archive.
    expect((await getUndoState(items)).undo?.opId).toBe(done.result?.opId);
    await undo(items);
    expect(daemon.items.get(x.id)?.status).toBe(Status.INBOX);
    expect(archived(x.id)?.reason).toBe(ArchiveReason.AUTO_DONE);

    // The next sweep heals it, whatever the threshold.
    const healed = daemon.runScheduler(100 * WEEK);
    expect([healed.healed, healed.archived]).toEqual([[x.id], []]);
    expect([healed.ops[0]?.summary, healed.ops[0]?.actor]).toEqual(["unarchive 1 reopened item", "system"]);
    expect(archived(x.id)).toBeUndefined();
    expect((await getUndoState(items)).redo?.opId).toBe(done.result?.opId);

    // Redo closes x again at its old close time. The scheduler's own archive
    // events do not make an archive stick, so it is archived again.
    await redo(items);
    at(2 * WEEK);
    expect(daemon.runScheduler().archived).toEqual([x.id]);
    expect((await getUndoState(items)).undo?.opId).toBe(done.result?.opId);
  });

  it("undo and redo move the close time back with the status", async () => {
    // Done on day 0 and cancelled on day 5: undoing the cancel closes x on day 0 again.
    {
      const { daemon, items, capture, at } = setup();
      const x = await capture("x");
      await setStatus(items, x.id, Status.DONE);
      at(5 * DAY);
      await setStatus(items, x.id, Status.CANCELLED);
      at(6 * DAY);
      await undo(items);
      at(WEEK + 1);
      expect(daemon.runScheduler().archived).toEqual([x.id]);
    }
    // ... and redoing it closes x on day 5 again.
    {
      const { daemon, items, capture, at } = setup();
      const x = await capture("x");
      await setStatus(items, x.id, Status.DONE);
      at(5 * DAY);
      await setStatus(items, x.id, Status.CANCELLED);
      at(6 * DAY);
      await undo(items);
      await redo(items);
      at(5 * DAY + WEEK - 1);
      expect(daemon.runScheduler().ops).toEqual([]);
      at(5 * DAY + WEEK);
      expect(daemon.runScheduler().archived).toEqual([x.id]);
    }
  });

  it("lets an undone archive go once a new op discards it", async () => {
    const { daemon, items, capture, at } = setup();
    const [x, y] = [await capture("x"), await capture("y")];
    await setStatus(items, x.id, Status.DONE);
    await archiveItem(items, x.id);
    await undo(items);
    // The undone archive is on the redo stack until this op discards it.
    await setStatus(items, y.id, Status.NEXT);
    expect(daemon.ops.some((o) => o.undoState === "discarded")).toBe(true);
    at(2 * WEEK);
    expect(daemon.runScheduler().archived).toEqual([x.id]);
  });

  it("heals in archive order, at most 1000 Items per op", () => {
    const { daemon } = setup();
    // Archived latest first, so archive order is the reverse of the Items'.
    const ids: string[] = [];
    for (let i = 0; i < 1001; i++) {
      const archived = { at: { seconds: BigInt(2000 - i) }, reason: ArchiveReason.AUTO_DONE };
      ids.push(daemon.put({ title: `reopened ${i}`, status: Status.NEXT, archived }).id);
    }
    const sweep = daemon.runScheduler();
    expect(sweep.healed).toEqual(ids.toReversed());
    expect(sweep.ops.map((o) => o.summary)).toEqual(["unarchive 1000 reopened items", "unarchive 1 reopened item"]);
  });

  it("heals and archives in one op, and SetStatus to an open status heals by itself", async () => {
    const { daemon, items, capture, at, archived } = setup();
    const [x, y, z] = [await capture("x"), await capture("y"), await capture("z")];
    await setStatus(items, x.id, Status.DONE);
    await setStatus(items, z.id, Status.DONE);
    at(WEEK + 1);
    expect(daemon.runScheduler().archived).toEqual([x.id, z.id]);
    await setStatus(items, y.id, Status.DONE);
    // Reopened through SetStatus, z leaves the auto_done archive in the same op.
    const reopened = await setStatus(items, z.id, Status.NEXT);
    expect(reopened.item?.archived).toBeUndefined();
    expect(daemon.ops.at(-1)?.events.map((e) => (e.kind === "set" ? e.field : e.kind))).toContain("archived");
    // An edit that reopens x without SetStatus (an undo) leaves it to the sweep.
    daemon.update(x.id, { status: Status.NEXT });
    at(2 * WEEK + 2);
    const sweep = daemon.runScheduler();
    expect([sweep.healed, sweep.archived, sweep.ops.map((o) => o.summary)]).toEqual([
      [x.id],
      [y.id],
      ["auto-archive 1 done item and unarchive 1 reopened item"],
    ]);
    expect(archived(y.id)?.reason).toBe(ArchiveReason.AUTO_DONE);
  });

  it("skips deleted Items, and changes at most 1000 Items per op", async () => {
    const { daemon } = setup();
    const gone = daemon.put({ title: "gone", status: Status.DONE, updatedAt: { seconds: 1n }, deletedAt: { seconds: 2n } });
    for (let i = 0; i < 1001; i++) daemon.put({ title: `old ${i}`, status: Status.DONE, updatedAt: { seconds: 1n } });
    const sweep = daemon.runScheduler();
    expect(sweep.archived).toHaveLength(1001);
    expect(sweep.archived).not.toContain(gone.id);
    expect(sweep.ops.map((o) => o.summary)).toEqual(["auto-archive 1000 done items", "auto-archive 1 done item"]);
  });

  it("never touches the undo or redo stack", async () => {
    const { daemon, items, capture, at } = setup();
    const x = await capture("x");
    await setStatus(items, x.id, Status.DONE);
    await undo(items);
    await redo(items);
    const before = await getUndoState(items);
    at(WEEK + 1);
    expect(daemon.runScheduler().archived).toEqual([x.id]);
    expect(await getUndoState(items)).toEqual(before);
    expect(daemon.ops.filter((o) => o.kind === OpKind.AUTO_ARCHIVE).map((o) => o.undoState)).toEqual([undefined]);
  });
});

// Generation cases the conformance suite cannot set up over RPC: a
// back-dated due date and a parent deleted behind the redo stack.
describe("FakeDaemon next instances", () => {
  it("names next year in the summary when the date's year is not today's", async () => {
    const { daemon, items, capture } = setup();
    const x = await capture("x @2nd");
    const dec2 = { year: 2026, month: 12, day: 2 };
    daemon.update(x.id, { dueDate: dec2, recurrence: { seriesId: x.recurrence!.seriesId, rule: x.recurrence!.rule, occurrenceDate: dec2 } });
    const res = await setStatus(items, x.id, Status.DONE);
    expect(res.generated?.dueDate).toMatchObject({ year: 2027, month: 1, day: 2 });
    expect((await getUndoState(items)).undo?.description).toBe('mark done "x" · next Sat Jan 2 2027');
  });

  it("starts an instance at the top level when its template is live under a deleted parent", async () => {
    const { daemon, items, capture } = setup();
    const p = await capture("p");
    const a = (await items.captureItem({ requestId: ulid(), text: "a @daily", createdVia: CreatedVia.TUI, parentId: p.id })).item!;
    daemon.update(p.id, { deletedAt: { seconds: 5n } });
    const made = (await setStatus(items, a.id, Status.DONE)).generated!;
    expect([made.parentId, made.parentTitle]).toEqual([undefined, undefined]);
    expect(daemon.items.get(a.id)?.parentId).toBe(p.id);
  });

  it("redo leaves the instance deleted when its parent has gone since", async () => {
    const { daemon, items, capture } = setup();
    const p = await capture("p");
    const a = (await items.captureItem({ requestId: ulid(), text: "a @daily", createdVia: CreatedVia.TUI, parentId: p.id })).item!;
    const made = (await setStatus(items, a.id, Status.DONE)).generated!;
    await undo(items);
    // Off-stack changes, so the redo stack survives.
    daemon.update(a.id, { parentId: undefined });
    daemon.update(p.id, { deletedAt: { seconds: 5n } });
    const redone = await redo(items);
    expect(redone.skipped.map((s) => [s.itemId, s.message])).toEqual([[made.id, "the parent is deleted"]]);
    expect(daemon.items.get(a.id)?.status).toBe(Status.DONE);
    expect(daemon.items.get(made.id)?.deletedAt).toBeDefined();
  });
});
