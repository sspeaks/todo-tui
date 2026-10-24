import { Code, ConnectError, createClient } from "@connectrpc/connect";
import { describe, expect, it } from "vitest";
import { ErrorInfoSchema } from "../src/gen/google/rpc/error_details_pb.js";
import { ArchiveReason, Priority, Status } from "../src/gen/todo/v1/item_pb.js";
import { ItemService } from "../src/gen/todo/v1/item_service_pb.js";
import { FakeDaemon } from "../src/fake-daemon.js";
import { archiveItem, deleteItem, setStatus, unarchiveItem, undeleteItem, updateItem } from "../src/mutations.js";

function setup() {
  const daemon = new FakeDaemon([{ title: "a", priority: Priority.HIGH, labels: ["x"] }, { title: "b" }]);
  const [a, b] = [...daemon.items.values()];
  return { daemon, client: createClient(ItemService, daemon.transport), a: a!, b: b! };
}

const failure = (p: Promise<unknown>) => p.then(() => undefined, (e: unknown) => ConnectError.from(e));

describe("FakeDaemon mutations", () => {
  it("fails a stale expected_revision with ABORTED / REVISION_MISMATCH and changes nothing", async () => {
    const { daemon, client, a } = setup();
    const before = daemon.revision;
    const err = await failure(updateItem(client, { id: a.id, title: "x" }, ["title"], { expectedRevision: a.revision + 1n }));
    expect(err?.code).toBe(Code.Aborted);
    const [info] = err?.findDetails(ErrorInfoSchema) ?? [];
    expect(info?.reason).toBe("REVISION_MISMATCH");
    expect(info?.metadata).toEqual({ expected_revision: String(a.revision + 1n), actual_revision: String(a.revision) });
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

  it("returns the original response for a replayed request id", async () => {
    const { daemon, client, a } = setup();
    const requestId = "01JREPLAY00000000000000000";
    const first = await setStatus(client, a.id, Status.NEXT, { requestId, expectedRevision: a.revision });
    await updateItem(client, { id: a.id, title: "changed since" }, ["title"]);
    const rev = daemon.revision;
    // Replays even though the Item and its revision have moved on.
    const again = await setStatus(client, a.id, Status.NEXT, { requestId, expectedRevision: a.revision });
    expect(again.result).toEqual(first.result);
    expect(again.item?.title).toBe("a");
    expect(daemon.revision).toBe(rev);
  });

  it("UpdateItem copies the masked top-level fields, clearing unset ones", async () => {
    const { client, a } = setup();
    const paths = ["title", "labels", "waiting.reason"];
    const res = await updateItem(client, { id: a.id, title: "renamed", priority: Priority.LOW }, paths);
    expect(res.item?.title).toBe("renamed");
    expect(res.item?.labels).toEqual([]);
    expect(res.item?.priority).toBe(Priority.HIGH);
    expect((await failure(updateItem(client, { id: a.id }, ["nope"])))?.code).toBe(Code.InvalidArgument);
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

  it("fails an unknown id with NOT_FOUND", async () => {
    const { client } = setup();
    expect((await failure(client.getItem({ id: "missing" })))?.code).toBe(Code.NotFound);
    expect((await failure(deleteItem(client, "missing")))?.code).toBe(Code.NotFound);
  });
});
