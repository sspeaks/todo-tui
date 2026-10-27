import { describe, expect, it } from "vitest";
import { editLine, emptyLine, insert, type LineState, lineOf, visibleLine } from "./line-editor.js";
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

  it("leaves keys that do not edit to the mode", () => {
    for (const k of ["enter", "esc", "tab", "up", "down", "fkey"]) expect(editLine(lineOf("x"), k)).toBeUndefined();
  });

  it("shows a window around the cursor", () => {
    expect(visibleLine(lineOf("abc"), 10)).toEqual({ before: "abc", under: " ", after: "" });
    expect(visibleLine({ text: "abc", cursor: 1 }, 10)).toEqual({ before: "a", under: "b", after: "c" });
    expect(visibleLine(lineOf("abcdefgh"), 4)).toEqual({ before: "fgh", under: " ", after: "" });
    expect(visibleLine({ text: "abcdefgh", cursor: 0 }, 4)).toEqual({ before: "", under: "a", after: "bcd" });
  });
});
