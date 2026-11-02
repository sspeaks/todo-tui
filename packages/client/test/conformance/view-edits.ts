// Editing saved views (design 2.4) as a suite both daemons run:
// test/view-edits.test.ts against FakeDaemon, uds.integration.test.ts
// against the real one. Each test gets a fresh daemon with the six
// defaults.
import type { Client, Transport } from "@connectrpc/connect";
import { Code, ConnectError, createClient } from "@connectrpc/connect";
import { afterEach, describe, expect, it } from "vitest";
import { BadRequestSchema, ErrorInfoSchema } from "../../src/gen/google/rpc/error_details_pb.js";
import { Topic } from "../../src/gen/todo/v1/common_pb.js";
import { ItemField } from "../../src/gen/todo/v1/field_pb.js";
import { Operator, RelativeDate_Anchor } from "../../src/gen/todo/v1/filter_pb.js";
import { ItemService } from "../../src/gen/todo/v1/item_service_pb.js";
import { Status } from "../../src/gen/todo/v1/item_pb.js";
import { ViewService } from "../../src/gen/todo/v1/view_service_pb.js";
import { WatchService } from "../../src/gen/todo/v1/watch_service_pb.js";
import { createView, deleteView, getUndoState, moveView, updateView } from "../../src/mutations.js";
import { ulid } from "../../src/ulid.js";

export interface ViewEditTarget {
  transport: Transport;
  close?: () => void | Promise<void>;
}

const failure = (p: Promise<unknown>) => p.then(() => undefined, (e: unknown) => ConnectError.from(e));

const next = { allOf: [{ kind: { case: "predicate" as const, value: { target: ItemField.STATUS, op: Operator.IN, operands: [{ kind: { case: "value" as const, value: { kind: { case: "status" as const, value: Status.NEXT } } } }] } } }] };

export function viewEditsConformance(make: () => Promise<ViewEditTarget>): void {
  let target: ViewEditTarget;
  let views: Client<typeof ViewService>;
  const setUp = async () => {
    target = await make();
    views = createClient(ViewService, target.transport);
  };
  afterEach(async () => {
    await target?.close?.();
  });
  const names = async () => {
    const list = (await views.listViews({})).views;
    expect(list.map((v) => v.position)).toEqual(list.map((_, i) => i));
    return list.map((v) => v.name);
  };
  const idOf = async (name: string) => (await views.listViews({})).views.find((v) => v.name === name)!.id;

  describe("view edits", () => {
    it("creates, renames, moves and deletes, keeping positions dense", async () => {
      await setUp();
      const created = await createView(views, { name: " Work ", filter: next, position: 1 });
      expect(created.view).toMatchObject({ name: "Work", position: 1, showCount: true });
      expect(created.view?.seed).toBeUndefined();
      expect(created.moved.map((v) => v.name)).toEqual(["Today", "Next", "Waiting", "Someday", "Archive"]);
      expect(await names()).toEqual(["Inbox", "Work", "Today", "Next", "Waiting", "Someday", "Archive"]);

      const work = created.view!;
      const renamed = await updateView(views, { id: work.id, name: "Deep work", showCount: false }, ["name"], { expectedRevision: work.revision });
      expect(renamed.view).toMatchObject({ name: "Deep work", showCount: true });
      expect(renamed.view!.revision).toBeGreaterThan(work.revision);

      const moved = await moveView(views, work.id, 99);
      expect(moved.view?.position).toBe(6);
      expect(moved.moved).toHaveLength(6);
      expect(await names()).toEqual(["Inbox", "Today", "Next", "Waiting", "Someday", "Archive", "Deep work"]);

      const today = await idOf("Today");
      const deleted = await deleteView(views, today);
      expect(deleted.moved.map((v) => v.name)).toEqual(["Next", "Waiting", "Someday", "Archive", "Deep work"]);
      expect(await names()).toEqual(["Inbox", "Next", "Waiting", "Someday", "Archive", "Deep work"]);
      // None of it is an undo entry.
      const undo = await getUndoState(createClient(ItemService, target.transport));
      expect(undo.undo).toBeUndefined();
    });

    it("refuses a taken name, a bad name, a bad filter, a bad mask and a stale revision", async () => {
      await setUp();
      const reason = async (p: Promise<unknown>) => {
        const err = await failure(p);
        return [err?.code, err?.findDetails(ErrorInfoSchema)[0]?.reason ?? err?.findDetails(BadRequestSchema)[0]?.fieldViolations[0]?.field];
      };
      expect(await reason(createView(views, { name: "INBOX", filter: next }))).toEqual([Code.AlreadyExists, "VIEW_NAME_TAKEN"]);
      expect(await reason(createView(views, { name: "  ", filter: next }))).toEqual([Code.InvalidArgument, "name"]);
      expect(await reason(createView(views, { name: "x".repeat(65), filter: next }))).toEqual([Code.InvalidArgument, "name"]);
      const lt = { allOf: [{ kind: { case: "predicate" as const, value: { target: ItemField.STATUS, op: Operator.LT, operands: [] } } }] };
      expect(await reason(createView(views, { name: "Bad", filter: lt }))).toEqual([Code.InvalidArgument, "filter.all_of[0].predicate.op"]);
      const inbox = (await views.listViews({})).views[0]!;
      expect(await reason(updateView(views, { id: inbox.id, name: "Next" }, ["name"]))).toEqual([Code.AlreadyExists, "VIEW_NAME_TAKEN"]);
      expect(await reason(views.updateView({ requestId: ulid(), view: { id: inbox.id }, updateMask: { paths: ["seed"] } }))).toEqual([Code.InvalidArgument, "update_mask.paths[0]"]);
      expect(await reason(updateView(views, { id: inbox.id, filter: lt }, ["filter"]))).toEqual([Code.InvalidArgument, "view.filter.all_of[0].predicate.op"]);
      expect(await reason(updateView(views, { id: inbox.id, showCount: false }, ["show_count"], { expectedRevision: inbox.revision + 1n }))).toEqual([Code.Aborted, "REVISION_MISMATCH"]);
      expect(await reason(deleteView(views, ulid()))).toEqual([Code.NotFound, "VIEW_NOT_FOUND"]);
      expect(await reason(moveView(views, "nope", 0))).toEqual([Code.InvalidArgument, "id"]);
    });

    it("checks in the daemon's order, reads only the masked fields, and names update paths under view", async () => {
      await setUp();
      const fields = async (p: Promise<unknown>) => {
        const err = await failure(p);
        return [err?.code, err?.findDetails(BadRequestSchema)[0]?.fieldViolations[0]?.field ?? err?.findDetails(ErrorInfoSchema)[0]?.metadata.field];
      };
      const lt = { allOf: [{ kind: { case: "predicate" as const, value: { target: ItemField.STATUS, op: Operator.LT, operands: [] } } }] };
      const unbuilt = { allOf: [{ kind: { case: "predicate" as const, value: { target: ItemField.REMOTE_EXTRAS, op: Operator.IS_SET, operands: [] } } }] };
      const far = { allOf: [{ kind: { case: "predicate" as const, value: { target: ItemField.DUE_DATE, op: Operator.LT, operands: [{ kind: { case: "relativeDate" as const, value: { anchor: RelativeDate_Anchor.TODAY, offsetDays: 40_000 } } }] } } }] };
      // An invalid filter before the name; an invalid sort before an unbuilt filter.
      expect(await fields(createView(views, { name: " ", filter: lt }))).toEqual([Code.InvalidArgument, "filter.all_of[0].predicate.op"]);
      expect(await fields(createView(views, { name: "X", filter: unbuilt, sort: [{ key: ItemField.LABELS }] }))).toEqual([Code.InvalidArgument, "sort[0].key"]);
      expect(await fields(createView(views, { name: "X", filter: unbuilt }))).toEqual([Code.Unimplemented, "filter.all_of[0].predicate.target"]);
      expect(await fields(createView(views, { name: "X", filter: far }))).toEqual([Code.InvalidArgument, "filter.all_of[0].predicate.operands[0].relative_date.offset_days"]);
      const inbox = (await views.listViews({})).views[0]!;
      expect(await fields(updateView(views, { id: inbox.id, filter: unbuilt }, ["filter"]))).toEqual([Code.Unimplemented, "view.filter.all_of[0].predicate.target"]);
      expect(await fields(updateView(views, { id: inbox.id, name: " " }, ["name"]))).toEqual([Code.InvalidArgument, "view.name"]);
      // A filter outside the mask is not read.
      const sorted = await updateView(views, { id: inbox.id, filter: unbuilt, sort: [{ key: ItemField.TITLE }] }, ["sort"]);
      expect(sorted.view?.sort).toHaveLength(1);
    });

    it("folds case in every script for names", async () => {
      await setUp();
      await createView(views, { name: "Café", filter: next });
      const err = await failure(createView(views, { name: "CAFÉ", filter: next }));
      expect(err?.findDetails(ErrorInfoSchema)[0]?.reason).toBe("VIEW_NAME_TAKEN");
    });

    it("replays a request id, and refuses one reused for another edit", async () => {
      await setUp();
      const requestId = ulid();
      const first = await createView(views, { name: "Work", filter: next }, { requestId });
      const again = await createView(views, { name: "Work", filter: next }, { requestId });
      expect(again.result).toEqual(first.result);
      expect(again.view?.id).toBe(first.view?.id);
      expect(await names()).toHaveLength(7);
      const err = await failure(deleteView(views, first.view!.id, { requestId }));
      expect(err?.findDetails(BadRequestSchema)[0]?.fieldViolations[0]?.field).toBe("request_id");
      // After the view is gone, the replay still answers its result.
      await deleteView(views, first.view!.id);
      const late = await createView(views, { name: "Work", filter: next }, { requestId });
      expect(late.result).toEqual(first.result);
      expect(late.view).toBeUndefined();
    });

    it("streams each view an edit moves on TOPIC_VIEW, a delete as deleted", async () => {
      await setUp();
      const watch = createClient(WatchService, target.transport);
      const ac = new AbortController();
      const seen: [string, boolean][] = [];
      let caughtUp = false;
      const done = (async () => {
        for await (const res of watch.subscribe({ topics: [Topic.VIEW] }, { signal: ac.signal })) {
          if (res.message.case === "caughtUp") caughtUp = true;
          if (res.message.case === "change") seen.push([res.message.value.id, res.message.value.deleted]);
          if (seen.length === 2) break;
        }
      })();
      while (!caughtUp) await new Promise((r) => setTimeout(r, 5));
      const someday = await idOf("Someday");
      const archive = await idOf("Archive");
      await deleteView(views, someday);
      await done;
      ac.abort();
      expect(seen).toEqual([
        [someday, true],
        [archive, false],
      ]);
    });
  });
}
