import { Code, ConnectError, createClient } from "@connectrpc/connect";
import { describe, expect, it } from "vitest";
import { BadRequestSchema, ErrorInfoSchema } from "../src/gen/google/rpc/error_details_pb.js";
import { ArchiveReason, CreatedVia, Priority, Status } from "../src/gen/todo/v1/item_pb.js";
import { ItemService } from "../src/gen/todo/v1/item_service_pb.js";
import { FakeDaemon } from "../src/fake-daemon.js";
import { MAX_CAPTURE_BYTES } from "../src/limits.js";
import { archiveItem, deleteItem, setStatus, unarchiveItem, undeleteItem, updateItem } from "../src/mutations.js";
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
    expect((await updateItem(client, { id: a.id, title: ` ${longest} ` }, ["title"])).item?.title).toBe(` ${longest} `);
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

  it("fails an unknown id with NOT_FOUND", async () => {
    const { client } = setup();
    const missing = ulid();
    expect((await failure(client.getItem({ id: missing })))?.code).toBe(Code.NotFound);
    const err = await failure(deleteItem(client, missing));
    expect(err?.code).toBe(Code.NotFound);
    expect(err?.findDetails(ErrorInfoSchema)[0]?.metadata).toEqual({ item_id: missing });
  });
});
