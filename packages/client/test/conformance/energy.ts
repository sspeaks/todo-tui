// The energy field (design 3.2, 4.8) as a suite any daemon runs: the %
// capture token, UpdateItem, filtering and sorting with ENERGY_NONE outside
// the order, and the copy onto the next instance of a series.
// test/energy-conformance.test.ts runs it against FakeDaemon,
// uds.integration.test.ts against the real daemon ($TODO_BIN). Each test
// takes a fresh daemon.
import { type MessageInitShape, create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import { afterEach, describe, expect, it } from "vitest";
import { BadRequestSchema } from "../../src/gen/google/rpc/error_details_pb.js";
import { ItemField as F } from "../../src/gen/todo/v1/field_pb.js";
import { type ClauseSchema, type FilterSchema, Operator as O, SortKeySchema } from "../../src/gen/todo/v1/filter_pb.js";
import { CreatedVia, Energy, Status } from "../../src/gen/todo/v1/item_pb.js";
import { CaptureTokenKind } from "../../src/gen/todo/v1/item_service_pb.js";
import { type ItemsClient, captureItem, setRecurrence, setStatus, updateItem } from "../../src/mutations.js";

/** A fresh daemon for one test. */
export interface EnergyTarget {
  items: ItemsClient;
  close?: () => void | Promise<void>;
}

type Clause = MessageInitShape<typeof ClauseSchema>;

const energy = (target: F, op: O, ...values: Energy[]): Clause => ({
  kind: {
    case: "predicate",
    value: { target, op, operands: values.map((e) => ({ kind: { case: "value", value: { kind: { case: "energy", value: e } } } })) },
  },
});

/** Registers the suite; `open` gives a fresh daemon. */
export function energyConformance(open: () => Promise<EnergyTarget>): void {
  describe("energy", () => {
    let close: (() => void | Promise<void>) | undefined;
    afterEach(async () => {
      await close?.();
      close = undefined;
    });

    async function target() {
      const t = await open();
      close = t.close;
      return t.items;
    }

    async function capture(items: ItemsClient, text: string) {
      const res = await captureItem(items, text, { createdVia: CreatedVia.CLI });
      return res.item!;
    }

    async function titles(items: ItemsClient, filter: MessageInitShape<typeof FilterSchema>, sort: { key: F; descending?: boolean }[] = []) {
      const res = await items.listItems({ filter, sort: sort.map((s) => create(SortKeySchema, s)) });
      return res.items.map((i) => i.title);
    }

    it("captures %low, %med and %high, case-insensitive, and keeps anything else in the title", async () => {
      const items = await target();
      expect((await capture(items, "a %low")).energy).toBe(Energy.LOW);
      expect((await capture(items, "b %MED")).energy).toBe(Energy.MEDIUM);
      expect((await capture(items, "c %medium")).energy).toBe(Energy.MEDIUM);
      const high = await captureItem(items, "d %High", { createdVia: CreatedVia.CLI });
      expect(high.item!.energy).toBe(Energy.HIGH);
      expect(high.draft!.energy).toBe(Energy.HIGH);
      expect(high.draft!.tokens.map((t) => [t.start, t.end, t.kind, t.applied])).toEqual([[2, 7, CaptureTokenKind.ENERGY, true]]);
      const plain = await captureItem(items, "e %huge", { createdVia: CreatedVia.CLI });
      expect(plain.item!.title).toBe("e %huge");
      expect(plain.item!.energy).toBe(Energy.NONE);
      expect(plain.draft!.warnings).toEqual(["%huge: not an energy level: use %low, %med or %high"]);
      expect((await capture(items, "grow 100% %")).title).toBe("grow 100% %");
    });

    it("sets and clears energy with UpdateItem; unset reads as none", async () => {
      const items = await target();
      const item = await capture(items, "x");
      expect(item.energy).toBe(Energy.NONE);
      const set = await updateItem(items, { id: item.id, energy: Energy.HIGH }, ["energy"]);
      expect(set.item!.energy).toBe(Energy.HIGH);
      const cleared = await updateItem(items, { id: item.id }, ["energy"]);
      expect(cleared.item!.energy).toBe(Energy.NONE);
      const err = await updateItem(items, { id: item.id, energy: 99 as Energy }, ["energy"]).then(
        () => undefined,
        (e: unknown) => ConnectError.from(e),
      );
      expect(err?.code).toBe(Code.InvalidArgument);
      const [bad] = err?.findDetails(BadRequestSchema) ?? [];
      expect([bad?.fieldViolations[0]?.field, bad?.fieldViolations[0]?.description]).toEqual(["item.energy", "unknown enum value 99"]);
    });

    it("refuses an ordering operator on ENERGY_NONE", async () => {
      const items = await target();
      const err = await items.listItems({ filter: { allOf: [energy(F.ENERGY, O.GT, Energy.NONE)] } }).then(
        () => undefined,
        (e: unknown) => ConnectError.from(e),
      );
      expect(err?.code).toBe(Code.InvalidArgument);
      const [bad] = err?.findDetails(BadRequestSchema) ?? [];
      expect(bad?.fieldViolations[0]?.field).toBe("filter.all_of[0].predicate.operands[0]");
    });

    it("filters and sorts by rank, with none outside the order and last both ways", async () => {
      const items = await target();
      for (const text of ["none", "high %high", "low %low", "medium %med"]) await capture(items, text);
      expect(await titles(items, { allOf: [energy(F.ENERGY, O.GE, Energy.MEDIUM)] }, [{ key: F.ENERGY }])).toEqual(["medium", "high"]);
      expect(await titles(items, { allOf: [energy(F.ENERGY, O.LT, Energy.HIGH)] }, [{ key: F.ENERGY }])).toEqual(["low", "medium"]);
      expect(await titles(items, { allOf: [energy(F.ENERGY, O.EQ, Energy.NONE)] })).toEqual(["none"]);
      expect(await titles(items, { allOf: [energy(F.ENERGY, O.IN, Energy.LOW, Energy.HIGH)] }, [{ key: F.ENERGY }])).toEqual(["low", "high"]);
      expect(await titles(items, {}, [{ key: F.ENERGY }])).toEqual(["low", "medium", "high", "none"]);
      expect(await titles(items, {}, [{ key: F.ENERGY, descending: true }])).toEqual(["high", "medium", "low", "none"]);
    });

    it("copies energy to the next instance of a series", async () => {
      const items = await target();
      const item = await capture(items, "water plants %low @daily");
      expect(item.energy).toBe(Energy.LOW);
      const done = await setStatus(items, item.id, Status.DONE);
      expect(done.generated?.energy).toBe(Energy.LOW);
      // A rule set later keeps it too.
      const other = await capture(items, "stretch %high");
      await setRecurrence(items, other.id, { kind: { case: "daily", value: {} } });
      const next = await setStatus(items, other.id, Status.DONE);
      expect(next.generated?.energy).toBe(Energy.HIGH);
    });
  });
}
