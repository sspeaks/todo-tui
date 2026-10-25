// The undo/redo, parent-delete and relation rules (design 3.6, 3.7, 3.10) as
// a suite any ItemService runs: test/undo-relations.test.ts runs it against
// FakeDaemon, uds.integration.test.ts against the real daemon ($TODO_BIN), so
// the fake keeps the daemon's rules. Summary texts are pinned here because
// clients show them ("Undid: <description>").
import type { MessageInitShape } from "@bufbuild/protobuf";
import { Code, ConnectError, type Transport } from "@connectrpc/connect";
import { afterEach, describe, expect, it } from "vitest";
import { BadRequestSchema, ErrorInfoSchema } from "../../src/gen/google/rpc/error_details_pb.js";
import { ItemField } from "../../src/gen/todo/v1/field_pb.js";
import { CreatedVia, type Item, type ItemSchema, Priority, ResolutionKind, Status, StatusCategory, TodoRefinement } from "../../src/gen/todo/v1/item_pb.js";
import { OpKind, UndoSkipReason } from "../../src/gen/todo/v1/op_pb.js";
import { type Relation, RelationKind } from "../../src/gen/todo/v1/relation_pb.js";
import { LiveStore } from "../../src/live-store.js";
import {
  type ItemsClient,
  addRelation,
  batchMutate,
  deleteItem,
  errorReason,
  getUndoState,
  redo,
  removeRelation,
  setStatus,
  undeleteItem,
  undo,
  updateItem,
} from "../../src/mutations.js";
import { ulid } from "../../src/ulid.js";

/** A daemon with an empty undo stack, for one test. */
export interface UndoTarget {
  items: ItemsClient;
  /** For a LiveStore on the same daemon. */
  transport: Transport;
  /**
   * Changes an Item off the undo stack, as a sync or system op would: not an
   * entry, and the redo stack stays. Unset where the daemon offers no way to
   * (every RPC is the user's), and the cases that need it are skipped.
   */
  offStack?: (id: string, patch: MessageInitShape<typeof ItemSchema>) => void | Promise<void>;
  close?: () => void | Promise<void>;
}

const ULID = /^[0-9A-HJKMNP-TV-Z]{26}$/;

async function waitFor(cond: () => boolean, timeoutMs = 5000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!cond()) {
    if (Date.now() > deadline) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 5));
  }
}

const failure = (p: Promise<unknown>) => p.then(() => undefined, (e: unknown) => ConnectError.from(e));

/** The ErrorInfo of a failure with `code`. */
async function info(p: Promise<unknown>, code: Code) {
  const err = await failure(p);
  expect(err?.code).toBe(code);
  return err?.findDetails(ErrorInfoSchema)[0];
}

/** The field of an INVALID_ARGUMENT failure's single BadRequest violation. */
async function badField(p: Promise<unknown>): Promise<string | undefined> {
  const err = await failure(p);
  expect(err?.code).toBe(Code.InvalidArgument);
  const [bad] = err?.findDetails(BadRequestSchema) ?? [];
  expect(bad?.fieldViolations).toHaveLength(1);
  return bad?.fieldViolations[0]?.field;
}

const ids = (items: readonly (Item | undefined)[]) => items.map((i) => i?.id);

const relationKeys = (relations: readonly Relation[]) => relations.map((r) => [r.fromId, r.toId, r.kind]);

/** Registers the suite; `open` gives each test a fresh daemon. */
export function undoRelationsConformance(open: () => Promise<UndoTarget>): void {
  const cleanups: (() => void | Promise<void>)[] = [];
  afterEach(async () => {
    for (const c of cleanups.splice(0).reverse()) await c();
  });

  async function setup() {
    const t = await open();
    if (t.close) cleanups.push(t.close);
    const items = t.items;
    const capture = async (text: string, parentId?: string) => {
      const res = await items.captureItem({ requestId: ulid(), text, createdVia: CreatedVia.TUI, ...(parentId !== undefined && { parentId }) });
      return { item: res.item!, op: res.result!.opId, revision: res.result!.revision };
    };
    const get = async (id: string) => (await items.getItem({ id })).item!;
    const state = () => getUndoState(items);
    const revision = async () => (await items.listItems({ pageSize: 1 })).revision;
    const live = async () => {
      const store = new LiveStore({ transport: t.transport, debounceMs: 20, backoff: { initialMs: 10, maxMs: 100 } });
      cleanups.push(() => store.stop());
      const view = store.view();
      store.start();
      await waitFor(() => store.getConnection().state === "live");
      return { store, view, shown: () => ids(view.getSnapshot().items) };
    };
    return { t, items, capture, get, state, revision, live };
  }

  describe("undo, redo, parent rules and relations", () => {
    it("undoes a capture by soft-deleting the Item, which the live store hides; redo brings it back", async () => {
      const { items, capture, get, state, live } = await setup();
      const { store, shown } = await live();
      const cap = await capture("call dentist");
      const id = cap.item.id;
      await waitFor(() => shown().includes(id));

      let s = await state();
      expect(s.undo).toMatchObject({ opId: cap.op, kind: OpKind.CAPTURE, itemIds: [id], description: 'capture "call dentist"' });
      expect(s.undo?.at).toBeDefined();
      expect(s.redo).toBeUndefined();

      const undone = await undo(items, { expectedOpId: cap.op });
      expect(undone.entry).toEqual(s.undo);
      expect(undone.skipped).toEqual([]);
      expect(ids(undone.items)).toEqual([id]);
      expect(undone.items[0]?.deletedAt).toBeDefined();
      expect(undone.result?.opId).toMatch(ULID);
      expect(undone.result?.opId).not.toBe(cap.op);
      expect(undone.result?.revision).toBe(cap.revision + 1n);
      await waitFor(() => !shown().includes(id));
      expect(store.getItem(id)?.deletedAt).toBeDefined();
      expect((await get(id)).deletedAt).toBeDefined();
      s = await state();
      expect(s.undo).toBeUndefined();
      expect(s.redo?.opId).toBe(cap.op);

      const redone = await redo(items, { expectedOpId: cap.op });
      expect(redone.entry?.opId).toBe(cap.op);
      expect(redone.skipped).toEqual([]);
      expect(ids(redone.items)).toEqual([id]);
      expect(redone.items[0]?.deletedAt).toBeUndefined();
      expect(redone.result?.revision).toBe(cap.revision + 2n);
      await waitFor(() => shown().includes(id));
      s = await state();
      expect(s.undo?.opId).toBe(cap.op);
      expect(s.redo).toBeUndefined();
    });

    it("undoes and redoes a status change as one unit of the status triple", async () => {
      const { items, capture, state } = await setup();
      const cap = await capture("a");
      const id = cap.item.id;
      expect(cap.item).toMatchObject({ status: Status.INBOX, statusCategory: StatusCategory.TODO, todoRefinement: TodoRefinement.INBOX });

      const done = await setStatus(items, id, Status.DONE);
      expect(done.item).toMatchObject({ status: Status.DONE, statusCategory: StatusCategory.DONE, resolutionKind: ResolutionKind.DONE });
      let s = await state();
      expect(s.undo).toMatchObject({ opId: done.result?.opId, kind: OpKind.SET_STATUS, itemIds: [id], description: 'mark done "a"' });

      const requestId = ulid();
      const undone = await undo(items, { requestId, expectedOpId: done.result!.opId });
      expect(undone.skipped).toEqual([]);
      expect(undone.items[0]).toMatchObject({ status: Status.INBOX, statusCategory: StatusCategory.TODO, todoRefinement: TodoRefinement.INBOX });
      expect(undone.items[0]?.resolutionKind).toBeUndefined();
      s = await state();
      expect(s.undo?.opId).toBe(cap.op);
      expect(s.redo?.opId).toBe(done.result?.opId);

      // A replay returns the original response and undoes nothing more.
      const replay = await undo(items, { requestId });
      expect(replay.result).toEqual(undone.result);
      expect(replay.entry).toEqual(undone.entry);
      expect(replay.skipped).toEqual([]);
      expect(ids(replay.items)).toEqual([id]);
      expect((await state()).undo?.opId).toBe(cap.op);
      // ... and the request id is an Undo's, not a Redo's.
      expect(await badField(redo(items, { requestId }))).toBe("request_id");

      const redone = await redo(items);
      expect(redone.entry?.opId).toBe(done.result?.opId);
      expect(redone.items[0]).toMatchObject({ status: Status.DONE, resolutionKind: ResolutionKind.DONE });
      s = await state();
      expect(s.undo?.opId).toBe(done.result?.opId);
      expect(s.redo).toBeUndefined();

      // In progress keeps the todo refinement, so undoing it restores only
      // the category; next and someday change the refinement.
      const next = await setStatus(items, id, Status.NEXT);
      const started = await setStatus(items, id, Status.IN_PROGRESS);
      expect(started.item).toMatchObject({ status: Status.IN_PROGRESS, todoRefinement: TodoRefinement.NEXT });
      expect((await state()).undo).toMatchObject({ opId: started.result?.opId, description: 'start "a"' });
      expect((await undo(items)).items[0]).toMatchObject({ status: Status.NEXT, todoRefinement: TodoRefinement.NEXT });
      expect((await state()).undo).toMatchObject({ opId: next.result?.opId, description: 'move to next "a"' });
      expect((await undo(items)).items[0]).toMatchObject({ status: Status.DONE, todoRefinement: TodoRefinement.INBOX });
    });

    it("undoes a batch of three status changes in one step", async () => {
      const { items, capture, state } = await setup();
      const [a, b, c] = [await capture("a"), await capture("b"), await capture("c")];
      const batch = await batchMutate(
        items,
        [a, b, c].map((x) => ({ kind: { case: "setStatus" as const, value: { id: x.item.id, status: Status.DONE } } })),
      );
      expect(batch.items.map((i) => i.status)).toEqual([Status.DONE, Status.DONE, Status.DONE]);
      expect(batch.result?.revision).toBe(c.revision + 3n);
      let s = await state();
      expect(s.undo).toMatchObject({
        opId: batch.result?.opId,
        kind: OpKind.BATCH,
        itemIds: ids([a.item, b.item, c.item]),
        description: "mark done (3 items)",
      });

      const undone = await undo(items, { expectedOpId: batch.result!.opId });
      expect(undone.entry?.description).toBe("mark done (3 items)");
      expect(undone.skipped).toEqual([]);
      // In the order of the undo's own changes: the batch's, reversed.
      expect(undone.items.map((i) => [i.id, i.status])).toEqual([
        [c.item.id, Status.INBOX],
        [b.item.id, Status.INBOX],
        [a.item.id, Status.INBOX],
      ]);
      expect(undone.result?.revision).toBe(c.revision + 6n);
      s = await state();
      expect(s.undo?.opId).toBe(c.op);
      expect(s.redo?.opId).toBe(batch.result?.opId);
    });

    it("undoes the entries last in, first out; undo, redo and undo round-trip", async () => {
      const { items, capture, get, state } = await setup();
      const cap = await capture("a");
      const id = cap.item.id;
      const toB = await updateItem(items, { id, title: "b" }, ["title"]);
      const toC = await updateItem(items, { id, title: "c" }, ["title"]);
      expect((await state()).undo).toMatchObject({ opId: toC.result?.opId, kind: OpKind.UPDATE, description: 'edit "c"' });
      const steps: [() => Promise<unknown>, string, string | undefined, string | undefined][] = [
        [() => undo(items), "b", toB.result?.opId, toC.result?.opId],
        [() => undo(items), "a", cap.op, toB.result?.opId],
        [() => redo(items), "b", toB.result?.opId, toC.result?.opId],
        [() => redo(items), "c", toC.result?.opId, undefined],
        [() => undo(items), "b", toB.result?.opId, toC.result?.opId],
      ];
      for (const [step, title, undoOp, redoOp] of steps) {
        await step();
        expect((await get(id)).title).toBe(title);
        const s = await state();
        expect([s.undo?.opId, s.redo?.opId]).toEqual([undoOp, redoOp]);
      }
    });

    it("guards Undo and Redo with expected_op_id", async () => {
      const { items, capture, get, state } = await setup();
      const a = await capture("a");
      const b = await capture("b");
      const stale = await info(undo(items, { expectedOpId: a.op }), Code.Aborted);
      expect(stale?.reason).toBe("UNDO_STACK_CHANGED");
      expect(stale?.metadata).toEqual({ expected_op_id: a.op, top_op_id: b.op });
      expect((await get(b.item.id)).deletedAt).toBeUndefined();
      expect((await state()).undo?.opId).toBe(b.op);

      // An empty redo stack has no top.
      const err = await failure(redo(items, { expectedOpId: b.op }));
      expect(err?.code).toBe(Code.Aborted);
      expect(errorReason(err)).toBe("UNDO_STACK_CHANGED");
      expect(err?.findDetails(ErrorInfoSchema)[0]?.metadata).toEqual({ expected_op_id: b.op, top_op_id: "" });

      expect((await undo(items, { expectedOpId: b.op })).entry?.opId).toBe(b.op);
      expect((await redo(items, { expectedOpId: b.op })).entry?.opId).toBe(b.op);
    });

    it("succeeds with no entry on an empty stack, and a late retry does not undo a newer op", async () => {
      const { items, capture, get, state, revision } = await setup();
      const s = await state();
      expect([s.undo, s.redo]).toEqual([undefined, undefined]);
      const rev = await revision();
      const requestId = ulid();
      const empty = await undo(items, { requestId });
      expect(empty.entry).toBeUndefined();
      expect(empty.items).toEqual([]);
      expect(empty.skipped).toEqual([]);
      expect(empty.result?.opId).toMatch(ULID);
      expect(empty.result?.revision).toBe(rev);
      const noRedo = await redo(items);
      expect(noRedo.entry).toBeUndefined();
      expect(noRedo.result?.revision).toBe(rev);
      const guarded = await info(undo(items, { expectedOpId: ulid() }), Code.Aborted);
      expect(guarded?.metadata?.top_op_id).toBe("");

      const a = await capture("a");
      const retry = await undo(items, { requestId });
      expect(retry.result).toEqual(empty.result);
      expect(retry.entry).toBeUndefined();
      expect((await get(a.item.id)).deletedAt).toBeUndefined();
      expect((await state()).undo?.opId).toBe(a.op);
    });

    it("a new entry clears the redo stack; a change of nothing is no entry and keeps it", async () => {
      const { items, capture, state } = await setup();
      const cap = await capture("a");
      const id = cap.item.id;
      const edit = await updateItem(items, { id, title: "b" }, ["title"]);
      await undo(items);
      expect((await state()).redo?.opId).toBe(edit.result?.opId);

      const same = await updateItem(items, { id, title: "a" }, ["title"]);
      const inbox = await setStatus(items, id, Status.INBOX);
      expect(same.result?.revision).toBe(inbox.result?.revision);
      let s = await state();
      expect([s.undo?.opId, s.redo?.opId]).toEqual([cap.op, edit.result?.opId]);

      const done = await setStatus(items, id, Status.DONE);
      s = await state();
      expect([s.undo?.opId, s.redo?.opId]).toEqual([done.result?.opId, undefined]);
      expect((await redo(items)).entry).toBeUndefined();
    });

    it("leaves a field changed since alone and reports it; a matching partial redo", async (ctx) => {
      const { t, items, capture, get, state, revision } = await setup();
      if (!t.offStack) return ctx.skip();
      const cap = await capture("x");
      const id = cap.item.id;
      const edit = await updateItem(items, { id, title: "b", priority: Priority.HIGH }, ["title", "priority"]);
      await t.offStack(id, { title: "c" });

      const undone = await undo(items, { expectedOpId: edit.result!.opId });
      expect(undone.entry?.opId).toBe(edit.result?.opId);
      expect(undone.skipped).toHaveLength(1);
      expect(undone.skipped[0]).toMatchObject({ itemId: id, field: ItemField.TITLE, reason: UndoSkipReason.CHANGED_SINCE });
      expect(undone.items[0]).toMatchObject({ id, title: "c", priority: Priority.NONE });
      expect((await state()).redo?.opId).toBe(edit.result?.opId);

      // Redo re-applies what the undo restored, and only that.
      const redone = await redo(items);
      expect(redone.skipped).toEqual([]);
      expect(redone.items[0]).toMatchObject({ id, title: "c", priority: Priority.HIGH });

      // An undo that restores nothing drops its entry from both stacks.
      const rename = await updateItem(items, { id, title: "d" }, ["title"]);
      await t.offStack(id, { title: "e" });
      const rev = await revision();
      const dropped = await undo(items, { expectedOpId: rename.result!.opId });
      expect(dropped.entry?.opId).toBe(rename.result?.opId);
      expect(dropped.items).toEqual([]);
      expect(dropped.skipped).toMatchObject([{ itemId: id, field: ItemField.TITLE, reason: UndoSkipReason.CHANGED_SINCE }]);
      expect(dropped.result?.revision).toBe(rev);
      expect((await get(id)).title).toBe("e");
      const s = await state();
      expect([s.undo?.opId, s.redo?.opId]).toEqual([edit.result?.opId, undefined]);
    });

    it("deletes the live subtasks with their parent; undo, redo and undelete bring them back", async () => {
      const { items, capture, get, state } = await setup();
      const p = await capture("p");
      const c = await capture("c", p.item.id);
      const g = await capture("g", c.item.id);
      expect(c.item.parentId).toBe(p.item.id);
      const all = ids([p.item, c.item, g.item]);

      const del = await deleteItem(items, p.item.id);
      const at = del.item?.deletedAt;
      expect(at).toBeDefined();
      expect(del.result?.revision).toBe(g.revision + 3n);
      for (const id of all) expect((await get(id!)).deletedAt).toEqual(at);
      expect((await state()).undo).toMatchObject({
        opId: del.result?.opId,
        kind: OpKind.DELETE,
        itemIds: ids([g.item, c.item, p.item]),
        description: 'delete "p" and 2 subtasks',
      });

      const undone = await undo(items);
      expect(ids(undone.items)).toEqual(all);
      expect(undone.items.map((i) => i.deletedAt)).toEqual([undefined, undefined, undefined]);
      expect(undone.skipped).toEqual([]);

      // Redo restores the recorded deleted_at, not the time of the redo.
      const redone = await redo(items);
      expect(ids(redone.items)).toEqual(ids([g.item, c.item, p.item]));
      for (const id of all) expect((await get(id!)).deletedAt).toEqual(at);

      const back = await undeleteItem(items, p.item.id);
      expect(back.item?.deletedAt).toBeUndefined();
      for (const id of all) expect((await get(id!)).deletedAt).toBeUndefined();
      expect((await state()).undo).toMatchObject({ opId: back.result?.opId, kind: OpKind.UNDELETE, description: 'undelete "p" and 2 subtasks' });
    });

    it("undeletes only the subtasks the parent's delete took along", async () => {
      const { items, capture, get, state } = await setup();
      const p = await capture("p");
      const c = await capture("c", p.item.id);
      await deleteItem(items, c.item.id);
      const del = await deleteItem(items, p.item.id);
      expect((await state()).undo).toMatchObject({ opId: del.result?.opId, description: 'delete "p"' });
      await undeleteItem(items, p.item.id);
      expect((await state()).undo?.description).toBe('undelete "p"');
      expect((await get(p.item.id)).deletedAt).toBeUndefined();
      expect((await get(c.item.id)).deletedAt).toBeDefined();
    });

    it("fails an undelete under a deleted parent with PARENT_DELETED, in a batch as the batch leaves it", async () => {
      const { items, capture, get, state, revision } = await setup();
      const p = await capture("p");
      const c = await capture("c", p.item.id);
      const del = await deleteItem(items, p.item.id);
      const refused = await info(undeleteItem(items, c.item.id), Code.FailedPrecondition);
      expect(refused?.reason).toBe("PARENT_DELETED");
      expect(refused?.metadata).toEqual({ item_id: c.item.id, parent_id: p.item.id });
      expect((await get(c.item.id)).deletedAt).toBeDefined();
      expect((await state()).undo?.opId).toBe(del.result?.opId);

      // The order of a batch's undeletes does not matter.
      const undelete = (id: string) => ({ kind: { case: "undeleteItem" as const, value: { id } } });
      const both = await batchMutate(items, [undelete(c.item.id), undelete(p.item.id)]);
      expect(both.items.map((i) => i.deletedAt)).toEqual([undefined, undefined]);
      expect((await state()).undo).toMatchObject({ opId: both.result?.opId, kind: OpKind.BATCH, description: "undelete 2 items" });
      // Its undo comes to the parent's delete while the subtask is still
      // live; that step waits for the subtask's.
      const undone = await undo(items, { expectedOpId: both.result!.opId });
      expect(undone.skipped).toEqual([]);
      expect(ids(undone.items)).toEqual([c.item.id, p.item.id]);
      expect(undone.items.map((i) => i.deletedAt !== undefined)).toEqual([true, true]);
      const redone = await redo(items);
      expect(redone.skipped).toEqual([]);
      expect(ids(redone.items)).toEqual([p.item.id, c.item.id]);
      expect(redone.items.map((i) => i.deletedAt)).toEqual([undefined, undefined]);

      // A batch that leaves one under a deleted parent applies nothing, and
      // names the mutation in the message.
      const rev = await revision();
      const err = await failure(batchMutate(items, [{ kind: { case: "deleteItem", value: { id: p.item.id } } }, undelete(c.item.id)]));
      expect(err?.code).toBe(Code.FailedPrecondition);
      expect(errorReason(err)).toBe("PARENT_DELETED");
      expect(err?.findDetails(ErrorInfoSchema)[0]?.metadata).toEqual({ item_id: c.item.id, parent_id: p.item.id });
      expect(err?.rawMessage).toMatch(/^mutations\[1\]\.undelete_item\.id: /);
      expect(await revision()).toBe(rev);
      expect((await get(p.item.id)).deletedAt).toBeUndefined();
      expect((await get(c.item.id)).deletedAt).toBeUndefined();
    });

    it("checks parent_id: it must name a live Item and not make a cycle", async () => {
      const { items, capture, get } = await setup();
      const p = await capture("p");
      const c = await capture("c", p.item.id);
      const gone = await capture("gone");
      await deleteItem(items, gone.item.id);
      const capturing = (parentId: string) => items.captureItem({ requestId: ulid(), text: "x", createdVia: CreatedVia.TUI, parentId });
      const moving = (parentId: string) => updateItem(items, { id: p.item.id, parentId }, ["parent_id"]);
      const description = async (call: Promise<unknown>) => (await failure(call))?.findDetails(BadRequestSchema)[0]?.fieldViolations[0];
      expect(await description(capturing("nope"))).toMatchObject({ field: "parent_id", description: "must be a ULID" });
      expect(await description(capturing(ulid()))).toMatchObject({ field: "parent_id", description: "no such Item" });
      expect(await description(capturing(gone.item.id))).toMatchObject({ field: "parent_id", description: "the parent is deleted" });
      expect(await description(moving(ulid()))).toMatchObject({ field: "item.parent_id", description: "no such Item" });
      expect(await description(moving(gone.item.id))).toMatchObject({ field: "item.parent_id", description: "the parent is deleted" });
      expect(await description(moving(c.item.id))).toMatchObject({ field: "item.parent_id", description: "would create a cycle" });
      expect(await description(moving(p.item.id))).toMatchObject({ field: "item.parent_id", description: "an Item cannot be its own parent" });
      expect((await get(p.item.id)).parentId).toBeUndefined();
    });

    it("adds and removes relations on both ends, as undoable ops", async () => {
      const { items, capture, get, state, revision } = await setup();
      const [a, b] = [(await capture("a")).item, (await capture("b")).item];
      const blocks = { fromId: a.id, toId: b.id, kind: RelationKind.BLOCKS };
      const rev = await revision();

      const add = await addRelation(items, blocks);
      expect(ids([add.fromItem, add.toItem])).toEqual([a.id, b.id]);
      for (const end of [add.fromItem, add.toItem]) expect(relationKeys(end!.relations)).toEqual([[a.id, b.id, RelationKind.BLOCKS]]);
      expect([add.fromItem?.revision, add.toItem?.revision, add.result?.revision]).toEqual([rev + 1n, rev + 2n, rev + 2n]);
      expect(relationKeys((await get(b.id)).relations)).toEqual([[a.id, b.id, RelationKind.BLOCKS]]);
      expect((await state()).undo).toMatchObject({
        opId: add.result?.opId,
        kind: OpKind.ADD_RELATION,
        itemIds: [a.id, b.id],
        description: 'relate "a" blocks "b"',
      });

      // A replay names the same relation, as stored; another is another request.
      const requestId = ulid();
      const first = await addRelation(items, { fromId: b.id, toId: a.id, kind: RelationKind.DUPLICATES }, { requestId });
      const replay = await addRelation(items, { fromId: b.id, toId: a.id, kind: RelationKind.DUPLICATES }, { requestId });
      expect(replay.result).toEqual(first.result);
      expect(await badField(addRelation(items, { fromId: b.id, toId: a.id, kind: RelationKind.BLOCKS }, { requestId }))).toBe("request_id");
      expect(await badField(removeRelation(items, { fromId: b.id, toId: a.id, kind: RelationKind.DUPLICATES }, { requestId }))).toBe("request_id");
      expect((await state()).undo).toMatchObject({ opId: first.result?.opId, description: 'relate "b" duplicates "a"' });
      await undo(items, { expectedOpId: first.result!.opId });

      // Adding it again, or removing one that does not exist, changes nothing.
      const settled = await revision();
      const again = await addRelation(items, blocks);
      expect(again.result?.revision).toBe(settled);
      expect(again.fromItem).toEqual(await get(a.id));
      const none = await removeRelation(items, { fromId: b.id, toId: a.id, kind: RelationKind.BLOCKS });
      expect(ids([none.fromItem, none.toItem])).toEqual([b.id, a.id]);
      expect(none.result?.revision).toBe(settled);
      expect((await state()).undo?.opId).toBe(add.result?.opId);

      // RELATES_TO is stored with from_id < to_id, whichever way it is asked.
      const [lo, hi] = a.id < b.id ? [a, b] : [b, a];
      const relates = await addRelation(items, { fromId: hi.id, toId: lo.id, kind: RelationKind.RELATES_TO });
      expect(ids([relates.fromItem, relates.toItem])).toEqual([lo.id, hi.id]);
      const both = [
        [a.id, b.id, RelationKind.BLOCKS],
        [lo.id, hi.id, RelationKind.RELATES_TO],
      ].sort((x, y) => (x[0] !== y[0] ? (x[0]! < y[0]! ? -1 : 1) : 0));
      expect(relationKeys(relates.fromItem!.relations)).toEqual(both);
      expect(relationKeys(relates.toItem!.relations)).toEqual(both);
      expect((await state()).undo?.description).toBe(`relate "${lo.title}" relates to "${hi.title}"`);

      // Undo removes it again, redo adds it back.
      const undone = await undo(items, { expectedOpId: relates.result!.opId });
      expect(undone.skipped).toEqual([]);
      expect(ids(undone.items)).toEqual([lo.id, hi.id]);
      for (const end of undone.items) expect(relationKeys(end.relations)).toEqual([[a.id, b.id, RelationKind.BLOCKS]]);
      const redone = await redo(items);
      expect(relationKeys(redone.items[0]!.relations)).toEqual(both);

      const removed = await removeRelation(items, blocks);
      expect(relationKeys(removed.fromItem!.relations)).toEqual([[lo.id, hi.id, RelationKind.RELATES_TO]]);
      expect((await state()).undo).toMatchObject({ opId: removed.result?.opId, kind: OpKind.REMOVE_RELATION, description: 'unrelate "a" blocks "b"' });
      await undo(items);
      expect(relationKeys((await get(a.id)).relations)).toEqual(both);

      // A relation outlives a deleted end, on both Items.
      await deleteItem(items, b.id);
      expect(relationKeys((await get(a.id)).relations)).toEqual(both);
      expect(relationKeys((await get(b.id)).relations)).toEqual(both);
      // Removing one with a deleted end is allowed.
      const unblocked = await removeRelation(items, blocks);
      expect(relationKeys(unblocked.toItem!.relations)).toEqual([[lo.id, hi.id, RelationKind.RELATES_TO]]);
    });

    it("rejects relations whose ends are missing, deleted or the same, naming the ends as the request does", async () => {
      const { items, capture } = await setup();
      const [a, b] = [(await capture("a")).item, (await capture("b")).item];
      const rel = (fromId: string, toId: string, kind = RelationKind.BLOCKS) => ({ fromId, toId, kind });
      expect(await badField(addRelation(items, rel(a.id, a.id)))).toBe("relation.to_id");
      expect(await badField(addRelation(items, rel(a.id, ulid())))).toBe("relation.to_id");
      expect(await badField(addRelation(items, rel(a.id, b.id, RelationKind.UNSPECIFIED)))).toBe("relation.kind");
      expect(await badField(addRelation(items, rel(a.id, "nope")))).toBe("relation.to_id");
      expect(await badField(addRelation(items, rel("nope", b.id)))).toBe("relation.from_id");
      const missing = ulid();
      expect((await info(addRelation(items, rel(missing, a.id)), Code.NotFound))?.reason).toBe("ITEM_NOT_FOUND");

      await deleteItem(items, b.id);
      for (const kind of [RelationKind.BLOCKS, RelationKind.RELATES_TO]) {
        expect((await info(addRelation(items, rel(b.id, a.id, kind)), Code.FailedPrecondition))?.metadata).toEqual({ item_id: b.id });
        expect(await badField(addRelation(items, rel(a.id, b.id, kind)))).toBe("relation.to_id");
      }

      // With both ends wrong, the request's from_id is named, whichever way
      // round a RELATES_TO is stored.
      const [lo, hi] = [ulid(), ulid()].sort();
      const [c, d] = [(await capture("c")).item, (await capture("d")).item].sort((x, y) => (x.id < y.id ? -1 : 1));
      await deleteItem(items, c!.id);
      await deleteItem(items, d!.id);
      for (const kind of [RelationKind.BLOCKS, RelationKind.RELATES_TO]) {
        for (const [fromId, toId] of [
          [lo!, hi!],
          [hi!, lo!],
        ]) {
          expect((await info(addRelation(items, rel(fromId!, toId!, kind)), Code.NotFound))?.metadata).toEqual({ item_id: fromId });
          expect((await info(removeRelation(items, rel(fromId!, toId!, kind)), Code.NotFound))?.metadata).toEqual({ item_id: fromId });
        }
        for (const [from, to] of [
          [c!, d!],
          [d!, c!],
        ]) {
          expect((await info(addRelation(items, rel(from!.id, to!.id, kind)), Code.FailedPrecondition))?.metadata).toEqual({ item_id: from!.id });
        }
      }
    });
  });
}
