import { Code, ConnectError, createClient, createRouterTransport } from "@connectrpc/connect";
import { describe, expect, it } from "vitest";
import { ErrorInfoSchema } from "../src/gen/google/rpc/error_details_pb.js";
import { CreatedVia, Status } from "../src/gen/todo/v1/item_pb.js";
import {
  type AddRelationRequest,
  type ArchiveItemRequest,
  type BatchMutateRequest,
  type CaptureItemRequest,
  type DeleteItemRequest,
  ItemService,
  type RedoRequest,
  type RemoveRelationRequest,
  type SetStatusRequest,
  type UnarchiveItemRequest,
  type UndeleteItemRequest,
  type UndoRequest,
  type UpdateItemRequest,
} from "../src/gen/todo/v1/item_service_pb.js";
import { RelationKind } from "../src/gen/todo/v1/relation_pb.js";
import { FakeDaemon } from "../src/fake-daemon.js";
import {
  addRelation,
  archiveItem,
  batchMutate,
  captureItem,
  deleteItem,
  errorReason,
  getUndoState,
  redo,
  removeRelation,
  setStatus,
  unarchiveItem,
  undeleteItem,
  undo,
  updateItem,
} from "../src/mutations.js";

const ULID = /^[0-9A-HJKMNP-TV-Z]{26}$/;

/** An ItemService client that records the requests it receives. */
function recorder() {
  const got = {
    updateItem: [] as UpdateItemRequest[],
    setStatus: [] as SetStatusRequest[],
    archiveItem: [] as ArchiveItemRequest[],
    unarchiveItem: [] as UnarchiveItemRequest[],
    deleteItem: [] as DeleteItemRequest[],
    undeleteItem: [] as UndeleteItemRequest[],
    batchMutate: [] as BatchMutateRequest[],
    undo: [] as UndoRequest[],
    redo: [] as RedoRequest[],
    getUndoState: 0,
    addRelation: [] as AddRelationRequest[],
    removeRelation: [] as RemoveRelationRequest[],
    captureItem: [] as CaptureItemRequest[],
  };
  const transport = createRouterTransport(({ service }) => {
    service(ItemService, {
      updateItem: (req) => (got.updateItem.push(req), {}),
      setStatus: (req) => (got.setStatus.push(req), {}),
      archiveItem: (req) => (got.archiveItem.push(req), {}),
      unarchiveItem: (req) => (got.unarchiveItem.push(req), {}),
      deleteItem: (req) => (got.deleteItem.push(req), {}),
      undeleteItem: (req) => (got.undeleteItem.push(req), {}),
      batchMutate: (req) => (got.batchMutate.push(req), {}),
      undo: (req) => (got.undo.push(req), {}),
      redo: (req) => (got.redo.push(req), {}),
      getUndoState: () => (got.getUndoState++, {}),
      addRelation: (req) => (got.addRelation.push(req), {}),
      removeRelation: (req) => (got.removeRelation.push(req), {}),
      captureItem: (req) => (got.captureItem.push(req), {}),
    });
  });
  return { client: createClient(ItemService, transport), got };
}

describe("mutation helpers", () => {
  it("UpdateItem builds the field mask from paths", async () => {
    const { client, got } = recorder();
    await updateItem(client, { id: "A", title: "new", dueDate: { year: 2026, month: 11, day: 2 } }, ["title", "due_date"], {
      expectedRevision: 7n,
    });
    const [req] = got.updateItem;
    expect(req?.requestId).toMatch(ULID);
    expect(req?.item?.id).toBe("A");
    expect(req?.item?.title).toBe("new");
    expect(req?.item?.dueDate?.day).toBe(2);
    expect(req?.updateMask?.paths).toEqual(["title", "due_date"]);
    expect(req?.expectedRevision).toBe(7n);
  });

  it("single-item helpers send the id, a fresh request id and expected_revision only when given", async () => {
    const { client, got } = recorder();
    await setStatus(client, "A", Status.DONE, { expectedRevision: 3n });
    await archiveItem(client, "B");
    await unarchiveItem(client, "C", { expectedRevision: 0n });
    await deleteItem(client, "D", { expectedRevision: 9n });
    await undeleteItem(client, "E");
    const reqs = [got.setStatus, got.archiveItem, got.unarchiveItem, got.deleteItem, got.undeleteItem].map((r) => r[0]!);
    expect(reqs.map((r) => [r.id, r.expectedRevision])).toEqual([
      ["A", 3n],
      ["B", undefined],
      ["C", 0n],
      ["D", 9n],
      ["E", undefined],
    ]);
    expect(got.setStatus[0]?.status).toBe(Status.DONE);
    for (const r of reqs) expect(r.requestId).toMatch(ULID);
    expect(new Set(reqs.map((r) => r.requestId)).size).toBe(reqs.length);
  });

  it("CaptureItem sends the text, created_via, a fresh request id, and parent_id and reference_time only when given", async () => {
    const { client, got } = recorder();
    await captureItem(client, "call dentist ^fri", { createdVia: CreatedVia.TUI });
    const at = new Date(Date.UTC(2026, 9, 26, 12));
    await captureItem(client, "child", { createdVia: CreatedVia.MCP, parentId: "P", referenceTime: at, requestId: "01JRETRY000000000000000000" });
    const [plain, child] = got.captureItem;
    expect(plain?.requestId).toMatch(ULID);
    expect([plain?.text, plain?.createdVia, plain?.parentId, plain?.referenceTime]).toEqual(["call dentist ^fri", CreatedVia.TUI, undefined, undefined]);
    expect([child?.text, child?.createdVia, child?.parentId, child?.requestId]).toEqual(["child", CreatedVia.MCP, "P", "01JRETRY000000000000000000"]);
    expect(child?.referenceTime?.seconds).toBe(BigInt(at.getTime() / 1000));
  });

  it("CaptureItem captures under a parent on FakeDaemon", async () => {
    const daemon = new FakeDaemon();
    const client = createClient(ItemService, daemon.transport);
    const parent = daemon.put({ title: "parent" });
    const res = await captureItem(client, "child", { createdVia: CreatedVia.TUI, parentId: parent.id });
    expect(res.item).toMatchObject({ title: "child", parentId: parent.id, createdVia: CreatedVia.TUI });
    expect(res.result?.revision).toBe(daemon.revision);
    await expect(captureItem(client, "orphan", { createdVia: CreatedVia.TUI, parentId: "01J00000000000000000000000" })).rejects.toMatchObject({
      code: Code.InvalidArgument,
    });
  });

  it("reuses a given request id, for retries", async () => {
    const { client, got } = recorder();
    const requestId = "01JRETRY000000000000000000";
    await deleteItem(client, "A", { requestId });
    await deleteItem(client, "A", { requestId });
    expect(got.deleteItem.map((r) => r.requestId)).toEqual([requestId, requestId]);
  });

  it("BatchMutate sets the batch request id and leaves the nested ones empty", async () => {
    const { client, got } = recorder();
    await batchMutate(client, [
      { kind: { case: "setStatus", value: { id: "A", status: Status.DONE, expectedRevision: 4n } } },
      { kind: { case: "updateItem", value: { item: { id: "B", title: "t" }, updateMask: { paths: ["title"] } } } },
      { kind: { case: "deleteItem", value: { id: "C" } } },
    ]);
    const [req] = got.batchMutate;
    expect(req?.requestId).toMatch(ULID);
    const kinds = req?.mutations.map((m) => m.kind) ?? [];
    expect(kinds.map((k) => k.case)).toEqual(["setStatus", "updateItem", "deleteItem"]);
    for (const k of kinds) expect(k.value?.requestId).toBe("");
    const [status, update] = kinds;
    expect(status?.case === "setStatus" && status.value.expectedRevision).toBe(4n);
    expect(update?.case === "updateItem" && update.value.updateMask?.paths).toEqual(["title"]);
  });

  it("Undo and Redo send a fresh request id and expected_op_id only when given", async () => {
    const { client, got } = recorder();
    const opId = "01JENTRY00000000000000000A";
    await undo(client, { expectedOpId: opId });
    await undo(client);
    await redo(client, { expectedOpId: opId, requestId: "01JRETRY000000000000000000" });
    await redo(client);
    await getUndoState(client);
    expect(got.undo.map((r) => r.expectedOpId)).toEqual([opId, undefined]);
    expect(got.redo.map((r) => [r.requestId, r.expectedOpId])).toEqual([
      ["01JRETRY000000000000000000", opId],
      [got.redo[1]?.requestId, undefined],
    ]);
    for (const r of [...got.undo, got.redo[1]!]) expect(r.requestId).toMatch(ULID);
    expect(got.getUndoState).toBe(1);
  });

  it("relation helpers send the relation with a fresh request id", async () => {
    const { client, got } = recorder();
    const relation = { fromId: "A", toId: "B", kind: RelationKind.BLOCKS };
    await addRelation(client, relation);
    await removeRelation(client, relation, { requestId: "01JRETRY000000000000000000" });
    expect(got.addRelation[0]?.relation).toMatchObject(relation);
    expect(got.addRelation[0]?.requestId).toMatch(ULID);
    expect(got.removeRelation[0]?.relation).toMatchObject(relation);
    expect(got.removeRelation[0]?.requestId).toBe("01JRETRY000000000000000000");
  });
});

describe("errorReason", () => {
  it("is the ErrorInfo reason of a ConnectError", () => {
    const info = new ConnectError("stale", Code.Aborted, undefined, [
      { desc: ErrorInfoSchema, value: { reason: "UNDO_STACK_CHANGED", domain: "todo-tui" } },
    ]);
    expect(errorReason(info)).toBe("UNDO_STACK_CHANGED");
    expect(errorReason(new ConnectError("plain", Code.Internal))).toBeUndefined();
    expect(errorReason(new Error("not a ConnectError"))).toBeUndefined();
    expect(errorReason(undefined)).toBeUndefined();
  });
});
