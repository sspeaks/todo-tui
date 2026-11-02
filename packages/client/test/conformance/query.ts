// Query text (design 4.8) as a suite: the default views' text on both
// daemons (test/query-conformance.test.ts runs FakeDaemon, which knows only
// that text), and the grammar against the real daemon alone
// (uds.integration.test.ts with `grammar`): the parser is the daemon's.
import { type Client, type Transport, createClient } from "@connectrpc/connect";
import { afterEach, describe, expect, it } from "vitest";
import { CreatedVia } from "../../src/gen/todo/v1/item_pb.js";
import { ItemService } from "../../src/gen/todo/v1/item_service_pb.js";
import { ViewService } from "../../src/gen/todo/v1/view_service_pb.js";
import { ulid } from "../../src/ulid.js";

export interface QueryTarget {
  transport: Transport;
  close?: () => void | Promise<void>;
}

export function queryConformance(make: () => Promise<QueryTarget>, opts: { grammar: boolean }): void {
  let target: QueryTarget;
  let views: Client<typeof ViewService>;
  let items: Client<typeof ItemService>;
  const setUp = async () => {
    target = await make();
    views = createClient(ViewService, target.transport);
    items = createClient(ItemService, target.transport);
  };
  afterEach(async () => {
    await target?.close?.();
  });

  describe("query text", () => {
    it("each view's query reads back to its filter, sort and count flag", async () => {
      await setUp();
      const { views: list } = await views.listViews({});
      expect(list.map((v) => v.query)).toContain("is:inbox sort:created_at");
      for (const v of list) {
        const res = await views.parseQuery({ text: v.query });
        expect(res.errors, v.name).toEqual([]);
        expect(res.filter, v.name).toEqual(v.filter);
        expect(res.sort, v.name).toEqual(v.sort);
        expect(res.showCount, v.name).toBe(v.showCount);
      }
    });

    it("counts what the query lists, as the view's tab count", async () => {
      await setUp();
      for (const text of ["a", "b"]) await items.captureItem({ requestId: ulid(), createdVia: CreatedVia.CLI, text });
      const { views: list } = await views.listViews({});
      const inbox = list.find((v) => v.seed === "inbox")!;
      const res = await views.parseQuery({ text: inbox.query, count: true });
      expect(res.count).toBe(2);
      expect((await views.parseQuery({ text: inbox.query })).count).toBeUndefined();
    });

    it.runIf(opts.grammar)("reports each problem with its span in code points", async () => {
      await setUp();
      const res = await views.parseQuery({ text: "é is:nope dew:1" });
      expect(res.filter).toBeUndefined();
      expect(res.errors.map((e) => [e.start, e.end])).toEqual([
        [2, 9],
        [10, 15],
      ]);
      expect(res.errors[1]?.message).toBe('unknown field "dew"');
    });

    it.runIf(opts.grammar)("reads groups, flags and sorts", async () => {
      await setUp();
      const res = await views.parseQuery({ text: "#work any:(is:next has:waiting) priority>=high include:done sort:-priority count:off" });
      expect(res.errors).toEqual([]);
      expect(res.filter?.allOf).toHaveLength(2);
      expect(res.filter?.anyOf).toHaveLength(2);
      expect(res.filter?.includeDone).toBe(true);
      expect(res.sort).toHaveLength(1);
      expect(res.showCount).toBe(false);
    });
  });
}
