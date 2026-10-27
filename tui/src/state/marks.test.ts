import { describe, expect, it } from "vitest";
import { anchorIndex, markedInOrder, pruneMarks, rangeMarks, toggleMark } from "./marks.js";

const ids = ["a", "b", "c", "d", "e"];

describe("marks", () => {
  it("toggles one mark", () => {
    const one = toggleMark(new Set(), "b");
    expect([...one]).toEqual(["b"]);
    expect([...toggleMark(one, "b")]).toEqual([]);
  });

  it("marks V's range from the anchor to the selection, either way, on top of the marks before it", () => {
    const visual = { anchorId: "c", anchor: 2, base: new Set(["a"]) };
    expect([...rangeMarks(visual, ids, 4)].sort()).toEqual(["a", "c", "d", "e"]);
    // Moving back past the anchor shrinks the range to the other side.
    expect([...rangeMarks(visual, ids, 1)].sort()).toEqual(["a", "b", "c"]);
    expect([...rangeMarks(visual, ids, 2)].sort()).toEqual(["a", "c"]);
    // The anchor's Item left: its old index stands in.
    expect(anchorIndex(visual, ["a", "b", "d", "e"])).toBe(2);
    expect([...rangeMarks(visual, ["a", "b", "d", "e"], 3)].sort()).toEqual(["a", "d", "e"]);
    expect([...rangeMarks(visual, [], -1)]).toEqual(["a"]);
  });

  it("prunes the marks whose Item left, keeping the same set when none did", () => {
    const marks = new Set(["a", "c"]);
    expect(pruneMarks(marks, new Set(ids))).toBe(marks);
    expect([...pruneMarks(marks, new Set(["c", "d"]))]).toEqual(["c"]);
  });

  it("lists the marked ids in list order", () => {
    expect(markedInOrder(new Set(["d", "a"]), ids)).toEqual(["a", "d"]);
    expect(markedInOrder(new Set(), ids)).toEqual([]);
  });
});
