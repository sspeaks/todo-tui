import { describe, expect, it } from "vitest";
import { altInLine, editLine, emptyLine, insert, type LineState, lineOf, visibleLine } from "./line-editor.js";
import { displayWidth } from "../format/text.js";
import { parseKeys } from "./tokenize.js";

const type = (line: LineState, spec: string) => parseKeys(spec).reduce((l, k) => editLine(l, k) ?? l, line);

describe("line editor", () => {
  it("inserts characters and pastes at the cursor, in code points", () => {
    expect(type(emptyLine, "ab<left>X")).toEqual({ text: "aXb", cursor: 2 });
    expect(type(lineOf("🎉party"), "<home><right>!")).toEqual({ text: "🎉!party", cursor: 2 });
    expect(insert({ text: "ad", cursor: 1 }, "bc")).toEqual({ text: "abcd", cursor: 3 });
    expect(type(emptyLine, "a<space>b")).toEqual({ text: "a b", cursor: 3 });
  });

  it("deletes back and forward, by word, to the start and to the end", () => {
    expect(type(lineOf("abc"), "<backspace>")).toEqual({ text: "ab", cursor: 2 });
    expect(type(lineOf("abc"), "<home><delete>")).toEqual({ text: "bc", cursor: 0 });
    expect(type(lineOf("abc"), "<home><backspace>")).toEqual({ text: "abc", cursor: 0 });
    expect(type(lineOf("call the  dentist  "), "<ctrl+w>")).toEqual({ text: "call the  ", cursor: 10 });
    expect(type(lineOf("call dentist"), "<left><left><ctrl+u>")).toEqual({ text: "st", cursor: 0 });
    expect(type(lineOf("call dentist"), "<home><ctrl+k>")).toEqual({ text: "", cursor: 0 });
    expect(type(lineOf("abc"), "<ctrl+a>x<ctrl+e>y")).toEqual({ text: "xabcy", cursor: 5 });
  });

  it("moves and deletes by word with alt (Alt+arrows, readline's alt-b/f/d, alt-backspace)", () => {
    expect(type(lineOf("call the dentist"), "<alt+left>")).toEqual({ text: "call the dentist", cursor: 9 });
    expect(type(lineOf("call the dentist"), "<alt+b><alt+b>X")).toEqual({ text: "call Xthe dentist", cursor: 6 });
    expect(type(lineOf("call the dentist"), "<home><alt+right>")).toEqual({ text: "call the dentist", cursor: 4 });
    expect(type(lineOf("call the dentist"), "<home><alt+f><ctrl+right>")).toEqual({ text: "call the dentist", cursor: 8 });
    expect(type(lineOf("call the dentist"), "<alt+backspace>")).toEqual({ text: "call the ", cursor: 9 });
    expect(type(lineOf("call the dentist"), "<home><alt+d>")).toEqual({ text: " the dentist", cursor: 0 });
    expect(type(lineOf("call the dentist"), "<home><ctrl+left><alt+delete><alt+delete>")).toEqual({ text: " dentist", cursor: 0 });
    // What other alt keys mean in a line: esc then the character (hard case 3), or nothing.
    expect(["alt+left", "alt+backspace", "alt+b", "alt+d"].map(altInLine)).toEqual(["edit", "edit", "edit", "edit"]);
    expect(["alt+j", "alt+space", "alt+up", "alt+enter", "j"].map(altInLine)).toEqual(["esc", "esc", "ignore", "ignore", undefined]);
  });

  it("leaves keys that do not edit to the mode", () => {
    for (const k of ["enter", "esc", "tab", "up", "down", "fkey"]) expect(editLine(lineOf("x"), k)).toBeUndefined();
  });

  it("shows a window around the cursor", () => {
    expect(visibleLine(lineOf("abc"), 10)).toEqual({ before: "abc", under: " ", after: "" });
    expect(visibleLine({ text: "abc", cursor: 1 }, 10)).toEqual({ before: "a", under: "b", after: "c" });
    expect(visibleLine(lineOf("abcdefgh"), 4)).toEqual({ before: "fgh", under: " ", after: "" });
    expect(visibleLine({ text: "abcdefgh", cursor: 0 }, 4)).toEqual({ before: "", under: "a", after: "bcd" });
  });

  it("scrolls by display cells: wide characters take two, so the cursor and the end stay in view", () => {
    const cjk = `${"日本語のタイトル".repeat(8)} END`;
    const end = visibleLine(lineOf(cjk), 20);
    expect(end.before.endsWith(" END")).toBe(true);
    expect(displayWidth(end.before) + 1).toBeLessThanOrEqual(20);
    expect(displayWidth(end.before) + 1).toBeGreaterThanOrEqual(19);
    // At the start, the window holds 20 cells, not 20 characters.
    const start = visibleLine({ text: cjk, cursor: 0 }, 20);
    expect(displayWidth(start.under + start.after)).toBe(20);
    // A wide character under the cursor, and emoji.
    expect(visibleLine({ text: "ab日cd", cursor: 2 }, 4)).toEqual({ before: "ab", under: "日", after: "" });
    expect(displayWidth(visibleLine(lineOf("🎉".repeat(30)), 11).before)).toBe(10);
  });
});
