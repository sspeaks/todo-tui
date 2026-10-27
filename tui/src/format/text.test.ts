import { describe, expect, it } from "vitest";
import { displayWidth, fit, sanitize, singleLine, truncate, wrap } from "./text.js";

describe("sanitize", () => {
  it("strips SGR, other CSI, OSC and lone escapes, and C0/C1 controls", () => {
    expect(sanitize("\x1b[31mred\x1b[0m")).toBe("red");
    expect(sanitize("a\x1b[2Jb\x1b[10;5Hc")).toBe("abc");
    expect(sanitize("link \x1b]8;;http://x\x07here\x1b]8;;\x1b\\ end")).toBe("link here end");
    expect(sanitize("bell\x07 nul\x00 del\x7f c1\x85\x9b31m end\x1b")).toBe("bell nul del c1 end");
    expect(sanitize("‮evil‬")).toBe("evil");
  });

  it("keeps line breaks only when multiline, and turns tabs into spaces", () => {
    expect(sanitize("a\r\nb\rc\td")).toBe("a b c d");
    expect(sanitize("a\r\nb\rc\td", { multiline: true })).toBe("a\nb\nc d");
  });

  it("singleLine collapses whitespace", () => {
    expect(singleLine("  a\n\n b\t c ")).toBe("a b c");
  });
});

describe("display width", () => {
  it("counts wide and zero-width characters", () => {
    expect(displayWidth("abc")).toBe(3);
    expect(displayWidth("日本")).toBe(4);
    expect(displayWidth("🎉x")).toBe(3);
    expect(displayWidth("é")).toBe(1);
    expect(displayWidth("👨‍👩‍👧")).toBe(2);
    expect(displayWidth("○◐●⊘⧗")).toBe(5);
  });

  it("truncates to a width with …, never splitting a wide character", () => {
    expect(truncate("hello world", 5)).toBe("hell…");
    expect(truncate("hello", 5)).toBe("hello");
    expect(truncate("日本語です", 5)).toBe("日本…");
    expect(displayWidth(truncate("日本語です", 4))).toBeLessThanOrEqual(4);
    expect(truncate("abc", 0)).toBe("");
    expect(fit("ab", 4)).toBe("ab  ");
    expect(fit("abcdef", 4)).toBe("abc…");
  });

  it("wraps words to a width and breaks words longer than a line", () => {
    expect(wrap("the quick brown fox", 9)).toEqual(["the quick", "brown fox"]);
    expect(wrap("a\n\nb", 5)).toEqual(["a", "", "b"]);
    expect(wrap("abcdefghij", 4)).toEqual(["abcd", "efgh", "ij"]);
  });
});
