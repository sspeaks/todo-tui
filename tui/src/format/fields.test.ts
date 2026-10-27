import { describe, expect, it } from "vitest";
import { commonPrefix, formatLabels, parseLabels, parseWaiting, sameLabels, waitingPrefix } from "./fields.js";

describe("the t and w texts", () => {
  it("reads labels as a set of words, # optional", () => {
    expect(formatLabels(["a", "b"])).toBe("#a #b");
    expect(parseLabels(" #a b ##c #a  ")).toEqual(["a", "b", "c"]);
    expect(parseLabels("# ")).toEqual([]);
    expect(sameLabels(["a", "b"], ["b", "a"])).toBe(true);
    expect(sameLabels(["a"], ["a", "b"])).toBe(false);
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
