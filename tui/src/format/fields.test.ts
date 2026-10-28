import { describe, expect, it } from "vitest";
import { applyLabelEdits, commonPrefix, formatLabels, parseLabelEdits, parseLabels, parseWaiting, sameLabels, waitingPrefix } from "./fields.js";

describe("the t and w texts", () => {
  it("reads labels as a set of words, # optional", () => {
    expect(formatLabels(["a", "b"])).toBe("#a #b");
    expect(parseLabels(" #a b #c #a  ")).toEqual(["a", "b", "c"]);
    expect(parseLabels("# ")).toEqual([]);
    // One # comes off, as capture reads it: a label that starts with # (from `x ##a#b`) survives the round trip.
    expect(parseLabels("##c")).toEqual(["#c"]);
    for (const labels of [["#urgent", "home"], ["#a#b"], ["#"], ["##x"]]) expect(parseLabels(formatLabels(labels))).toEqual(labels);
    expect(parseLabelEdits("+##a -##b")).toEqual({ add: ["#a"], remove: ["#b"] });
    expect(sameLabels(["a", "b"], ["b", "a"])).toBe(true);
    expect(sameLabels(["a"], ["a", "b"])).toBe(false);
  });

  it("reads bulk t's +a -b and applies it to each Item's own labels", () => {
    expect(parseLabelEdits(" +a -b #c d -#e + -")).toEqual({ add: ["a", "c", "d"], remove: ["b", "e"] });
    // The last word for a label wins.
    expect(parseLabelEdits("+a -a")).toEqual({ add: [], remove: ["a"] });
    expect(parseLabelEdits("-a a")).toEqual({ add: ["a"], remove: [] });
    const edits = parseLabelEdits("+urgent -later");
    expect(applyLabelEdits(["later", "home"], edits)).toEqual(["home", "urgent"]);
    expect(applyLabelEdits(["urgent"], edits)).toEqual(["urgent"]);
    expect(applyLabelEdits([], parseLabelEdits(""))).toEqual([]);
  });

  it("reads <person> [^date], the person with spaces, only a last ^word a date", () => {
    expect(parseWaiting("dana")).toEqual({ person: "dana" });
    expect(parseWaiting("the vendor ^fri ")).toEqual({ person: "the vendor", date: "^fri" });
    expect(parseWaiting("^fri")).toEqual({ person: "", date: "^fri" });
    expect(parseWaiting("a^b c")).toEqual({ person: "a^b c" });
    expect(waitingPrefix("dan ^fri")).toBe("dan");
    expect(waitingPrefix("dan")).toBe("dan");
  });

  it("finds the prefix names share, without case", () => {
    expect(commonPrefix(["Dana Scully", "dave"])).toBe("Da");
    expect(commonPrefix(["Mulder"])).toBe("Mulder");
    expect(commonPrefix([])).toBe("");
  });
});
