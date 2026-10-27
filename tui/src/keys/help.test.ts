import { describe, expect, it } from "vitest";
import { bindingKeys, helpBindings, helpLines } from "./help.js";
import { BINDINGS, bindingModes, isAvailable } from "./keymap.js";

describe("help", () => {
  it("lists every binding that runs now once, and none that does not (hard case 6)", () => {
    const text = helpLines().map((l) => l.text);
    const rows = helpBindings();
    const listed = rows.flat().map((b) => b.id);
    const runnable = BINDINGS.filter((b) => bindingModes(b).includes("normal") && isAvailable(b)).map((b) => b.id);
    expect(listed.sort()).toEqual([...runnable].sort());
    for (const [first] of rows) {
      const row = text.filter((t) => t.includes(bindingKeys(first!)) && t.endsWith(first!.help));
      expect(row, first!.id).toHaveLength(1);
    }
    expect(text.some((t) => t.includes("Mark done"))).toBe(false);
    expect(text.find((t) => t.includes("Go to view N"))).toMatch(/1–9/);
    expect(text.find((t) => t.includes("First row"))).toMatch(/gg home/);
  });

  it("adds the commands and the capture and date cheat sheets", () => {
    const text = helpLines().map((l) => l.text).join("\n");
    expect(text).toMatch(/:quit :q\s+Quit/);
    expect(text).toContain(":refresh");
    expect(text).not.toContain(":search");
    expect(text).toMatch(/\^<date>\s+due date/);
    expect(text).toMatch(/eow eom/);
  });
});
