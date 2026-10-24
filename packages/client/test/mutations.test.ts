import { createClient, createRouterTransport } from "@connectrpc/connect";
import { describe, expect, it } from "vitest";
import { Status } from "../src/gen/todo/v1/item_pb.js";
import {
  type ArchiveItemRequest,
  type BatchMutateRequest,
  type DeleteItemRequest,
  ItemService,
  type SetStatusRequest,
  type UnarchiveItemRequest,
  type UndeleteItemRequest,
  type UpdateItemRequest,
} from "../src/gen/todo/v1/item_service_pb.js";
import {
  archiveItem,
  batchMutate,
  deleteItem,
  setStatus,
  unarchiveItem,
  undeleteItem,
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
});
