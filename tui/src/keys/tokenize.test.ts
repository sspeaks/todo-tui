import { describe, expect, it } from "vitest";
import { altKey, isChar, parseKeys, sequenceLabel, tokenize } from "./tokenize.js";

describe("tokenize", () => {
  it("splits a burst into one token per key (hard case 1)", () => {
    expect(tokenize("jjj", {})).toEqual(["j", "j", "j"]);
    expect(tokenize("milk\rbread\r", {})).toEqual(["m", "i", "l", "k", "enter", "b", "r", "e", "a", "d", "enter"]);
    expect(tokenize("a b\n", {})).toEqual(["a", "space", "b", "enter"]);
    expect(tokenize("😀x", {})).toEqual(["😀", "x"]);
    expect(tokenize("j\x04k", {})).toEqual(["j", "ctrl+d", "k"]);
  });

  it("reads a run of line breaks in one chunk as one enter (CRLF, blank lines)", () => {
    expect(tokenize("milk\r\n\r\nbread\r\n", {})).toEqual(["m", "i", "l", "k", "enter", "b", "r", "e", "a", "d", "enter"]);
    expect(tokenize("\r\r", {})).toEqual(["enter"]);
  });

  it("reads meta+key as alt+key, which the dispatcher takes as esc then the key outside a line (hard case 3)", () => {
    expect(tokenize("j", { meta: true })).toEqual(["alt+j"]);
    expect(tokenize("", { escape: true, meta: true })).toEqual(["esc", "esc"]);
    expect(tokenize("", { upArrow: true, meta: true })).toEqual(["alt+up"]);
    expect(tokenize("", { backspace: true, meta: true })).toEqual(["alt+backspace"]);
    expect(tokenize(" ", { meta: true })).toEqual(["alt+space"]);
    // ctrl with it, or a burst: esc, then the keys.
    expect(tokenize("", { leftArrow: true, ctrl: true, meta: true })).toEqual(["esc", "ctrl+left"]);
    expect(tokenize("jk", { meta: true })).toEqual(["esc", "j", "k"]);
    expect(altKey("alt+b")).toBe("b");
    expect(altKey("b")).toBeUndefined();
  });

  it("names flagged keys", () => {
    expect(tokenize("\r", { return: true })).toEqual(["enter"]);
    expect(tokenize("", { escape: true })).toEqual(["esc"]);
    expect(tokenize("", { tab: true })).toEqual(["tab"]);
    expect(tokenize("", { tab: true, shift: true })).toEqual(["shift+tab"]);
    expect(tokenize("", { downArrow: true })).toEqual(["down"]);
    expect(tokenize("", { pageDown: true })).toEqual(["pagedown"]);
    expect(tokenize("", { backspace: true })).toEqual(["backspace"]);
    expect(tokenize("d", { ctrl: true })).toEqual(["ctrl+d"]);
    expect(tokenize("c", { ctrl: true })).toEqual(["ctrl+c"]);
    expect(tokenize("G", { shift: true })).toEqual(["G"]);
  });

  it("takes an empty input with no flag as a function key (hard case 6)", () => {
    expect(tokenize("", {})).toEqual(["fkey"]);
  });

  it("parses key specs for tests", () => {
    expect(parseKeys("jj<esc>k<ctrl+d><lt>x")).toEqual(["j", "j", "esc", "k", "ctrl+d", "<", "x"]);
    expect(parseKeys(":q\r")).toEqual([":", "q", "enter"]);
  });

  it("tells characters from named keys and labels sequences", () => {
    expect(["j", "space", "😀"].every(isChar)).toBe(true);
    expect(["esc", "ctrl+d", "enter"].some(isChar)).toBe(false);
    expect(sequenceLabel(["g", "g"])).toBe("gg");
    expect(sequenceLabel(["ctrl+d"])).toBe("ctrl-d");
    expect(sequenceLabel(["down"])).toBe("↓");
    expect(sequenceLabel(["fkey"])).toBe("F1");
  });
});
