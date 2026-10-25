import { create } from "@bufbuild/protobuf";
import { timestampNow } from "@bufbuild/protobuf/wkt";
import { Code, ConnectError, createClient } from "@connectrpc/connect";
import { describe, expect, it } from "vitest";
import { BadRequestSchema, ErrorInfoSchema } from "../src/gen/google/rpc/error_details_pb.js";
import { ItemField } from "../src/gen/todo/v1/field_pb.js";
import { ArchiveReason, CreatedVia, Priority, ResolutionKind, Status, StatusCategory } from "../src/gen/todo/v1/item_pb.js";
import { ItemService } from "../src/gen/todo/v1/item_service_pb.js";
import { OpKind, UndoSkipReason, UndoSkipSchema } from "../src/gen/todo/v1/op_pb.js";
import { RelationKind } from "../src/gen/todo/v1/relation_pb.js";
import { ViewService } from "../src/gen/todo/v1/view_service_pb.js";
import { FakeDaemon } from "../src/fake-daemon.js";
import { MAX_CAPTURE_BYTES } from "../src/limits.js";
import {
  addRelation,
  archiveItem,
  batchMutate,
  deleteItem,
  getUndoState,
  redo,
  setStatus,
  unarchiveItem,
  undeleteItem,
  undo,
  updateItem,
} from "../src/mutations.js";
import { ulid } from "../src/ulid.js";

function setup() {
  const daemon = new FakeDaemon([{ title: "a", priority: Priority.HIGH, labels: ["x"] }, { title: "b" }]);
  const [a, b] = [...daemon.items.values()];
  return { daemon, client: createClient(ItemService, daemon.transport), a: a!, b: b! };
}

const failure = (p: Promise<unknown>) => p.then(() => undefined, (e: unknown) => ConnectError.from(e));

/** The field of an INVALID_ARGUMENT error's single BadRequest violation. */
async function badField(p: Promise<unknown>): Promise<string | undefined> {
  const err = await failure(p);
  expect(err?.code).toBe(Code.InvalidArgument);
  const [bad] = err?.findDetails(BadRequestSchema) ?? [];
  expect(bad?.fieldViolations).toHaveLength(1);
  return bad?.fieldViolations[0]?.field;
}

describe("FakeDaemon mutations", () => {
  it("fails a stale expected_revision with ABORTED / REVISION_MISMATCH and changes nothing", async () => {
    const { daemon, client, a } = setup();
    const before = daemon.revision;
    const err = await failure(updateItem(client, { id: a.id, title: "x" }, ["title"], { expectedRevision: a.revision + 1n }));
    expect(err?.code).toBe(Code.Aborted);
    const [info] = err?.findDetails(ErrorInfoSchema) ?? [];
    expect(info?.reason).toBe("REVISION_MISMATCH");
    expect(info?.metadata).toEqual({
      item_id: a.id,
      expected_revision: String(a.revision + 1n),
      actual_revision: String(a.revision),
    });
    expect(daemon.revision).toBe(before);
    expect(daemon.items.get(a.id)?.title).toBe("a");

    for (const call of [
      setStatus(client, a.id, Status.DONE, { expectedRevision: 0n }),
      archiveItem(client, a.id, { expectedRevision: 0n }),
      unarchiveItem(client, a.id, { expectedRevision: 0n }),
      deleteItem(client, a.id, { expectedRevision: 0n }),
      undeleteItem(client, a.id, { expectedRevision: 0n }),
    ]) {
      expect((await failure(call))?.code).toBe(Code.Aborted);
    }

    const ok = await updateItem(client, { id: a.id, title: "x" }, ["title"], { expectedRevision: a.revision });
    expect(ok.item?.title).toBe("x");
    expect(ok.item?.revision).toBe(before + 1n);
    expect(ok.result?.revision).toBe(before + 1n);
  });

  it("replays a request id with the original result and the current state", async () => {
    const { daemon, client, a, b } = setup();
    const requestId = ulid();
    const first = await setStatus(client, a.id, Status.NEXT, { requestId, expectedRevision: a.revision });
    await updateItem(client, { id: a.id, title: "changed since" }, ["title"]);
    const rev = daemon.revision;
    // Replays even though the Item and its revision have moved on.
    const again = await setStatus(client, a.id, Status.NEXT, { requestId: requestId.toLowerCase(), expectedRevision: a.revision });
    expect(again.result).toEqual(first.result);
    expect(again.item?.title).toBe("changed since");
    expect(daemon.revision).toBe(rev);

    // The same request id for another RPC or another Item is a different request.
    for (const call of [archiveItem(client, a.id, { requestId }), setStatus(client, b.id, Status.NEXT, { requestId })]) {
      expect(await badField(call)).toBe("request_id");
    }
    expect(await badField(client.captureItem({ requestId, text: "c", createdVia: CreatedVia.TUI }))).toBe("request_id");
    expect(daemon.revision).toBe(rev);
  });

  it("fails every mutation of a deleted Item except UndeleteItem, and DeleteItem, a no-op", async () => {
    const { daemon, client, a } = setup();
    const deleted = await deleteItem(client, a.id);
    for (const call of [
      updateItem(client, { id: a.id, title: "zombie" }, ["title"]),
      setStatus(client, a.id, Status.DONE),
      archiveItem(client, a.id),
      unarchiveItem(client, a.id),
    ]) {
      const err = await failure(call);
      expect(err?.code).toBe(Code.FailedPrecondition);
      const [info] = err?.findDetails(ErrorInfoSchema) ?? [];
      expect(info?.reason).toBe("ITEM_DELETED");
      expect(info?.metadata).toEqual({ item_id: a.id });
    }
    const again = await deleteItem(client, a.id);
    expect(again.item).toEqual(deleted.item);
    expect(again.result?.revision).toBe(deleted.result?.revision);
    expect((await undeleteItem(client, a.id)).item?.deletedAt).toBeUndefined();
    expect(daemon.revision).toBe((deleted.result?.revision ?? 0n) + 1n);
  });

  it("changes nothing, and allocates no revision, for a mutation that changes nothing", async () => {
    const { daemon, client, a } = setup();
    const archived = await archiveItem(client, a.id);
    const changes = daemon.changes.length;
    const rev = daemon.revision;
    for (const call of [
      archiveItem(client, a.id),
      undeleteItem(client, a.id),
      setStatus(client, a.id, a.status),
      updateItem(client, { id: a.id, title: "a", labels: ["x"] }, ["title", "labels"]),
    ]) {
      const res = await call;
      expect(res.item).toEqual(archived.item);
      expect(res.result?.revision).toBe(rev);
    }
    expect(daemon.revision).toBe(rev);
    expect(daemon.changes).toHaveLength(changes);
  });

  it("UpdateItem copies the masked top-level fields, clearing unset ones", async () => {
    const { client, a } = setup();
    const paths = ["title", "labels", "waiting.reason"];
    const res = await updateItem(client, { id: a.id, title: "renamed", priority: Priority.LOW }, paths);
    expect(res.item?.title).toBe("renamed");
    expect(res.item?.labels).toEqual([]);
    expect(res.item?.priority).toBe(Priority.HIGH);
  });

  it("rejects malformed requests with BadRequest details, as the daemon does", async () => {
    const { client, a } = setup();
    const cases: [Promise<unknown>, string][] = [
      [archiveItem(client, a.id, { requestId: "" }), "request_id"],
      [archiveItem(client, a.id, { requestId: "nope" }), "request_id"],
      [archiveItem(client, "nope"), "id"],
      [updateItem(client, { id: "nope", title: "x" }, ["title"]), "item.id"],
      [updateItem(client, { id: a.id }, []), "update_mask"],
      [updateItem(client, { id: a.id }, ["nope"]), "update_mask"],
      [updateItem(client, { id: a.id, status: Status.DONE }, ["status"]), "item.status"],
      [updateItem(client, { id: a.id }, ["archived"]), "item.archived"],
      [updateItem(client, { id: a.id }, ["relations"]), "item.relations"],
      [client.captureItem({ requestId: ulid(), text: " ", createdVia: CreatedVia.TUI }), "text"],
      [client.getItem({ id: "nope" }), "id"],
    ];
    for (const [call, field] of cases) expect(await badField(call)).toBe(field);
  });

  it("limits capture text and titles to MAX_CAPTURE_BYTES of UTF-8, as the daemon does", async () => {
    const { daemon, client, a } = setup();
    // Two bytes per character, then one byte more.
    const longest = "é".repeat(MAX_CAPTURE_BYTES / 2);
    const over = `${longest}x`;
    const capture = (text: string) => client.captureItem({ requestId: ulid(), text, createdVia: CreatedVia.TUI });
    expect((await capture(longest)).item?.title).toBe(longest);
    const err = await failure(capture(over));
    expect(err?.rawMessage).toBe("text: is longer than 64 KiB");
    expect(await badField(capture(over))).toBe("text");
    expect(await badField(updateItem(client, { id: a.id, title: over }, ["title"]))).toBe("item.title");
    expect(daemon.items.get(a.id)?.title).toBe("a");
    // Counted, and stored, trimmed.
    expect((await updateItem(client, { id: a.id, title: ` ${longest} ` }, ["title"])).item?.title).toBe(longest);
  });

  it("sets status, archives and unarchives", async () => {
    const { client, a } = setup();
    expect((await setStatus(client, a.id, Status.DONE)).item?.status).toBe(Status.DONE);
    expect((await archiveItem(client, a.id)).item?.archived?.reason).toBe(ArchiveReason.USER);
    expect((await unarchiveItem(client, a.id)).item?.archived).toBeUndefined();
  });

  it("soft-deletes: GetItem still returns the Item, ListItems hides it unless include_deleted", async () => {
    const { daemon, client, a, b } = setup();
    const changes = daemon.changes.length;
    const del = await deleteItem(client, a.id);
    expect(del.item?.deletedAt).toBeDefined();
    expect(daemon.changes.slice(changes).map((c) => [c.id, c.deleted])).toEqual([[a.id, false]]);

    const got = await client.getItem({ id: a.id });
    expect(got.item?.deletedAt).toBeDefined();
    expect(got.revision).toBe(daemon.revision);
    expect((await client.listItems({})).items.map((i) => i.id)).toEqual([b.id]);
    expect((await client.listItems({ filter: { includeDeleted: true } })).items.map((i) => i.id)).toEqual([a.id, b.id]);

    expect((await undeleteItem(client, a.id)).item?.deletedAt).toBeUndefined();
    expect((await client.listItems({})).items.map((i) => i.id)).toEqual([a.id, b.id]);
  });

  it("ListItems hides archived, done and deferred Items unless asked for them", async () => {
    const today = new Date();
    const tomorrow = new Date(today.getFullYear(), today.getMonth(), today.getDate() + 1);
    const daemon = new FakeDaemon([
      { title: "keep" },
      { title: "archived", archived: { reason: ArchiveReason.USER } },
      { title: "done", status: Status.DONE },
      { title: "cancelled", status: Status.CANCELLED },
      { title: "deferred", deferUntil: { year: tomorrow.getFullYear(), month: tomorrow.getMonth() + 1, day: tomorrow.getDate() } },
      {
        title: "due again",
        deferUntil: { year: today.getFullYear(), month: today.getMonth() + 1, day: today.getDate() },
      },
    ]);
    const client = createClient(ItemService, daemon.transport);
    const titles = async (filter = {}) => (await client.listItems({ filter })).items.map((i) => i.title);
    expect(await titles()).toEqual(["keep", "due again"]);
    expect(await titles({ includeArchived: true })).toEqual(["keep", "archived", "due again"]);
    expect(await titles({ includeDone: true })).toEqual(["keep", "done", "cancelled", "due again"]);
    expect(await titles({ includeDeferred: true })).toEqual(["keep", "deferred", "due again"]);
  });

  it("CaptureItem replays its request id", async () => {
    const { daemon, client } = setup();
    const req = { requestId: ulid(), text: "once", createdVia: CreatedVia.TUI };
    const first = await client.captureItem(req);
    const again = await client.captureItem(req);
    expect(again.result).toEqual(first.result);
    expect(again.item?.id).toBe(first.item?.id);
    expect([...daemon.items.values()].filter((i) => i.title === "once")).toHaveLength(1);
  });

  it("answers the RPCs it does not model with UNIMPLEMENTED", async () => {
    const { daemon, client } = setup();
    const views = createClient(ViewService, daemon.transport);
    for (const call of [client.parseDate({ text: "fri" }), client.getStatusCounts({}), views.listViews({})]) {
      expect((await failure(call))?.code).toBe(Code.Unimplemented);
    }
  });

  it("fails an unknown id with NOT_FOUND", async () => {
    const { client } = setup();
    const missing = ulid();
    expect((await failure(client.getItem({ id: missing })))?.code).toBe(Code.NotFound);
    const err = await failure(deleteItem(client, missing));
    expect(err?.code).toBe(Code.NotFound);
    expect(err?.findDetails(ErrorInfoSchema)[0]?.metadata).toEqual({ item_id: missing });
  });
});

/** A FakeDaemon with an ItemService client and capture / state helpers. */
function undoSetup() {
  const daemon = new FakeDaemon();
  const client = createClient(ItemService, daemon.transport);
  const capture = async (text: string, parentId?: string) => {
    const res = await client.captureItem({ requestId: ulid(), text, createdVia: CreatedVia.TUI, ...(parentId !== undefined && { parentId }) });
    return res.item!;
  };
  const state = async () => {
    const s = await getUndoState(client);
    return [s.undo?.description, s.redo?.description];
  };
  return { daemon, client, capture, state };
}

describe("FakeDaemon op log and undo", () => {
  it("makes entries of the user's ops that changed something; system ops and no-ops keep the redo stack", async () => {
    const { daemon, client, capture, state } = undoSetup();
    const seed = daemon.put({ title: "seed" });
    const a = await capture("a");
    const edit = await updateItem(client, { id: a.id, title: "b" }, ["title"]);
    await undo(client);
    expect(await state()).toEqual(['capture "a"', 'edit "b"']);

    await setStatus(client, a.id, Status.INBOX);
    daemon.update(seed.id, { title: "seed 2" });
    expect(await state()).toEqual(['capture "a"', 'edit "b"']);
    await archiveItem(client, a.id);
    expect(await state()).toEqual(['archive "a"', undefined]);

    expect(daemon.ops.map((o) => [o.kind, o.actor, o.undoState])).toEqual([
      [OpKind.UNSPECIFIED, "system", undefined],
      [OpKind.CAPTURE, "user", "done"],
      [OpKind.UPDATE, "user", "discarded"],
      [OpKind.UNDO, "user", undefined],
      [OpKind.SET_STATUS, "user", undefined],
      [OpKind.UNSPECIFIED, "system", undefined],
      [OpKind.ARCHIVE, "user", "done"],
    ]);
    const undoOp = daemon.ops[3]!;
    expect(undoOp).toMatchObject({ entryOp: edit.result?.opId, summary: 'undo: edit "b"', skipped: [] });
    expect(undoOp.events).toEqual([{ kind: "set", itemId: a.id, field: "title", old: "b", new: "a" }]);
  });

  it("restores the status triple as one unit and reports it once, as STATUS", async () => {
    const { daemon, client, capture, state } = undoSetup();
    const a = await capture("a");
    await setStatus(client, a.id, Status.DONE);
    daemon.update(a.id, { status: Status.CANCELLED });
    expect(daemon.items.get(a.id)).toMatchObject({ statusCategory: StatusCategory.DONE, resolutionKind: ResolutionKind.CANCELLED });
    const res = await undo(client);
    expect(res.skipped).toEqual([
      create(UndoSkipSchema, {
        itemId: a.id,
        field: ItemField.STATUS,
        reason: UndoSkipReason.CHANGED_SINCE,
        message: "the field has changed since",
      }),
    ]);
    // Nothing was restored, so the entry is dropped.
    expect(res.items).toEqual([]);
    expect(daemon.items.get(a.id)?.status).toBe(Status.CANCELLED);
    expect(await state()).toEqual(['capture "a"', undefined]);
  });

  it("matches by value: labels as a set, and a value changed and changed back", async () => {
    const { daemon, client, capture } = undoSetup();
    const a = await capture("a");
    await updateItem(client, { id: a.id, labels: ["x", "y"] }, ["labels"]);
    daemon.update(a.id, { labels: ["y", "x"] });
    await updateItem(client, { id: a.id, title: "b" }, ["title"]);
    daemon.update(a.id, { title: "c" });
    daemon.update(a.id, { title: "b" });
    expect((await undo(client)).items[0]?.title).toBe("a");
    expect((await undo(client)).items[0]?.labels).toEqual([]);
  });

  it("restores the recorded values exactly, and leaves a deleted Item's fields alone", async () => {
    const { daemon, client, capture, state } = undoSetup();
    const a = await capture("a");
    const archived = (await archiveItem(client, a.id)).item?.archived;
    await unarchiveItem(client, a.id);
    await new Promise((r) => setTimeout(r, 5));
    expect((await undo(client)).items[0]?.archived).toEqual(archived);

    const b = await capture("b");
    await updateItem(client, { id: b.id, title: "b2" }, ["title"]);
    daemon.update(b.id, { deletedAt: timestampNow() });
    const res = await undo(client);
    expect(res.skipped).toMatchObject([{ itemId: b.id, field: ItemField.TITLE, reason: UndoSkipReason.ITEM_DELETED, message: "the Item is deleted" }]);
    expect(await state()).toEqual(['capture "b"', undefined]);
  });

  it("reports a value that no longer fits as INVALID", async () => {
    const { daemon, client, capture } = undoSetup();
    const reason = async (step: typeof undo | typeof redo = undo) => {
      const [skip] = (await step(client)).skipped;
      return [skip?.field, skip?.reason, skip?.message];
    };
    // A parent deleted since.
    const p = await capture("p");
    const x = await capture("x");
    await updateItem(client, { id: x.id, parentId: p.id }, ["parent_id"]);
    await undo(client);
    daemon.update(p.id, { deletedAt: timestampNow() });
    expect(await reason(redo)).toEqual([ItemField.PARENT_ID, UndoSkipReason.INVALID, "the parent is deleted"]);
    // An Item that is gone.
    const gone = await capture("gone");
    daemon.remove(gone.id);
    expect(await reason()).toEqual([ItemField.DELETED_AT, UndoSkipReason.INVALID, "no such Item"]);
    // A capture that has live subtasks now.
    const q = await capture("q");
    daemon.put({ title: "child", parentId: q.id });
    expect(await reason()).toEqual([ItemField.DELETED_AT, UndoSkipReason.INVALID, "the Item has live subtasks"]);
    // A relation step whose end is gone.
    const [a, b] = [await capture("a"), await capture("b")];
    await addRelation(client, { fromId: a.id, toId: b.id, kind: RelationKind.BLOCKS });
    daemon.remove(b.id);
    const res = await undo(client);
    expect(res.skipped).toEqual([
      create(UndoSkipSchema, {
        itemId: a.id,
        reason: UndoSkipReason.INVALID,
        message: `item ${b.id} does not exist`,
        relation: { fromId: a.id, toId: b.id, kind: RelationKind.BLOCKS },
      }),
    ]);
  });

  it("undoes and redoes relation steps on a deleted end", async () => {
    const { daemon, client, capture } = undoSetup();
    const [a, b] = [await capture("a"), await capture("b")];
    await addRelation(client, { fromId: a.id, toId: b.id, kind: RelationKind.BLOCKS });
    daemon.update(b.id, { deletedAt: timestampNow() });
    const res = await undo(client);
    expect(res.skipped).toEqual([]);
    expect(res.items.map((i) => [i.id, i.relations.length])).toEqual([
      [a.id, 0],
      [b.id, 0],
    ]);
    expect((await redo(client)).items.map((i) => i.relations.length)).toEqual([1, 1]);
  });
});

describe("FakeDaemon BatchMutate", () => {
  const status = (id: string, s = Status.DONE, extra = {}) => ({ kind: { case: "setStatus" as const, value: { id, status: s, ...extra } } });
  const update = (id: string, title: string, extra = {}) => ({
    kind: { case: "updateItem" as const, value: { item: { id, title }, updateMask: { paths: ["title"] }, ...extra } },
  });

  it("applies all or nothing, naming the failed mutation's fields", async () => {
    const { daemon, client, a, b } = setup();
    const rev = daemon.revision;
    const over = "x".repeat(MAX_CAPTURE_BYTES + 1);
    expect(await badField(batchMutate(client, [status(a.id), update(b.id, over)]))).toBe("mutations[1].update_item.item.title");
    expect(await badField(batchMutate(client, [status(a.id, Status.DONE, { requestId: ulid() })]))).toBe("mutations[0].set_status.request_id");
    expect(await badField(batchMutate(client, [{ kind: { case: undefined } }]))).toBe("mutations[0].kind");
    expect(await badField(batchMutate(client, [status(a.id), status("nope")]))).toBe("mutations[1].set_status.id");
    expect(await badField(batchMutate(client, []))).toBe("mutations");
    expect(await badField(batchMutate(client, Array.from({ length: 1001 }, () => status(a.id))))).toBe("mutations");
    const parent = { kind: { case: "updateItem" as const, value: { item: { id: a.id, parentId: a.id }, updateMask: { paths: ["parent_id"] } } } };
    expect(await badField(batchMutate(client, [parent]))).toBe("mutations[0].update_item.item.parent_id");

    const deleted = await failure(batchMutate(client, [{ kind: { case: "deleteItem", value: { id: a.id } } }, update(a.id, "zombie")]));
    expect(deleted?.code).toBe(Code.FailedPrecondition);
    expect(deleted?.rawMessage).toBe(`mutations[1].update_item.item.id: item ${a.id} is deleted`);
    const stale = await failure(batchMutate(client, [status(a.id), status(b.id, Status.DONE, { expectedRevision: 0n })]));
    expect(stale?.code).toBe(Code.Aborted);
    expect(daemon.revision).toBe(rev);
    expect(daemon.items.get(a.id)?.status).toBe(Status.INBOX);
  });

  it("checks expected_revision against the Item before the batch and gives each touched Item one revision", async () => {
    const { daemon, client, a } = setup();
    const res = await batchMutate(client, [update(a.id, "x", { expectedRevision: a.revision }), update(a.id, "y", { expectedRevision: a.revision })]);
    expect(res.items).toHaveLength(2);
    expect(res.items[0]).toEqual(res.items[1]);
    expect(res.items[0]?.title).toBe("y");
    expect(res.result?.revision).toBe(a.revision + 2n);
    expect(daemon.revision).toBe(a.revision + 2n);
    expect((await getUndoState(client)).undo?.description).toBe('edit "y"');
  });

  it("replays by request id alone", async () => {
    const { client, a, b } = setup();
    const requestId = ulid();
    const first = await batchMutate(client, [status(a.id)], { requestId });
    const again = await batchMutate(client, [status(b.id)], { requestId });
    expect(again.result).toEqual(first.result);
    expect(again.items[0]?.id).toBe(b.id);
    expect(again.items[0]?.status).toBe(Status.INBOX);
    expect(await badField(setStatus(client, a.id, Status.DONE, { requestId }))).toBe("request_id");
  });

  it("words its summary as the daemon does", async () => {
    const { client, a, b } = setup();
    const summary = async (mutations: Parameters<typeof batchMutate>[1]) => {
      await batchMutate(client, mutations);
      return (await getUndoState(client)).undo?.description;
    };
    expect(await summary([status(a.id), status(b.id, Status.NEXT)])).toBe("change 2 items");
    expect(await summary([update(a.id, "x"), update(b.id, "y")])).toBe("edit 2 items");
    expect(await summary([update(a.id, "z"), status(a.id, Status.SOMEDAY)])).toBe('change "z"');
    const long = "é".repeat(41);
    expect(await summary([update(a.id, long)])).toBe(`edit "${"é".repeat(39)}…"`);
    const archive = (id: string) => ({ kind: { case: "archiveItem" as const, value: { id } } });
    expect(await summary([archive(a.id), archive(b.id)])).toBe("archive 2 items");
  });
});
