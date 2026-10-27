import { describe, expect, it } from "vitest";
import { SCROLLOFF, halfPage, initialList, type ListState, listOf, move, reconcile, select } from "./selection.js";

const ids = (n: number, from = 0) => Array.from({ length: n }, (_, i) => `id${i + from}`);

describe("reconcile", () => {
  it("keeps the selection on its Item when the list changes around it", () => {
    const list = listOf(select(initialList, ids(5), 10, 2));
    expect(reconcile(list, ["new", ...ids(5)], 10)).toMatchObject({ selectedId: "id2", index: 3 });
  });

  it("falls back to the same index when the Item leaves, or the last row (hard cases 12, 14)", () => {
    const list = listOf(select(initialList, ids(5), 10, 2));
    expect(reconcile(list, ["id0", "id1", "id3", "id4"], 10)).toMatchObject({ selectedId: "id3", index: 2 });
    const last = listOf(select(initialList, ids(5), 10, 4));
    expect(reconcile(last, ids(4), 10)).toMatchObject({ selectedId: "id3", index: 3 });
  });

  it("selects nothing in an empty list, and row 0 when rows return", () => {
    const list = listOf(select(initialList, ids(5), 10, 3));
    const empty = listOf(reconcile(list, [], 10));
    expect(reconcile(list, [], 10).index).toBe(-1);
    expect(reconcile(list, [], 10).selectedId).toBeUndefined();
    expect(reconcile(empty, ["a", "b"], 10)).toMatchObject({ selectedId: "a", index: 0 });
  });

  it("anchors the scroll on the top row, so inserts above move nothing on screen (hard case 13)", () => {
    const all = ids(2000);
    const at500 = listOf(move(initialList, all, 20, 500));
    expect(at500).toMatchObject({ selectedId: "id500" });
    const top = at500.topId;
    const after = reconcile(at500, ["early", ...all], 20);
    expect(after).toMatchObject({ selectedId: "id500", topId: top, offset: at500.offset + 1 });
  });

  it("keeps the scroll when the top row leaves", () => {
    const all = ids(100);
    const list = listOf(move(initialList, all, 10, 50));
    const without = all.filter((id) => id !== list.topId);
    expect(reconcile(list, without, 10)).toMatchObject({ selectedId: "id50", offset: list.offset });
  });

  it("keeps SCROLLOFF rows around the selection, and re-clamps for a smaller window", () => {
    const all = ids(100);
    let list: ListState = initialList;
    for (let i = 0; i < 9; i++) list = listOf(move(list, all, 10, 1));
    expect(list).toMatchObject({ selectedId: "id9", offset: 9 + SCROLLOFF - 9 });
    list = listOf(move(list, all, 10, -8));
    expect(list).toMatchObject({ selectedId: "id1", offset: 0 });
    const deep = listOf(select(initialList, all, 30, 25));
    const shrunk = reconcile(deep, all, 8);
    expect(shrunk.index - shrunk.offset).toBeGreaterThanOrEqual(SCROLLOFF);
    expect(shrunk.index - shrunk.offset).toBeLessThan(8 - SCROLLOFF);
  });

  it("moves by half pages, keeping the selection's screen line, clamped at the ends", () => {
    const all = ids(100);
    const start = listOf(select(initialList, all, 20, 5));
    const down = halfPage(start, all, 20, 1);
    expect(down).toMatchObject({ index: 15, offset: 10 });
    expect(halfPage(listOf(down), all, 20, -1)).toMatchObject({ index: 5, offset: 0 });
    expect(halfPage(listOf(select(initialList, all, 20, 99)), all, 20, 1)).toMatchObject({ index: 99, offset: 80 });
    expect(select(initialList, all, 20, 1000)).toMatchObject({ index: 99, offset: 80 });
  });
});
