import { describe, expect, it } from "vitest";
import { bindingKeys, helpBindings, helpLines, matchRuns, searchWords } from "./help.js";
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
    expect(text.find((t) => t.includes("Mark done"))).toMatch(/^ {2}x /);
    expect(text.find((t) => t.includes("Mark a range"))).toMatch(/^ {2}V /);
    expect(text.find((t) => t.includes("act on every marked Item"))).toMatch(/^ {2}x X s p e \^ @ D w W /);
    expect(text.find((t) => t.includes("Go to view N"))).toMatch(/1–9/);
    expect(text.find((t) => t.includes("First row"))).toMatch(/gg home/);
  });

  it("adds the commands and the capture, repeat rule and date cheat sheets", () => {
    const text = helpLines().map((l) => l.text).join("\n");
    expect(text).toMatch(/:quit :q\s+Quit/);
    expect(text).toContain(":refresh");
    expect(text).toMatch(/:search :s <query>\s+Search everything/);
    expect(text).toMatch(/\^<date>\s+due date/);
    expect(text).toMatch(/@<rule>\s+repeats/);
    expect(text).toMatch(/3d 2w\s+days, weeks after done/);
    expect(text).toMatch(/eow eom/);
  });

  it("narrows to the rows that hold every word, in any case, each under its section", () => {
    const lines = helpLines(BINDINGS, "  DUE date ");
    const rows = lines.filter((l) => l.kind === "row").map((l) => l.text);
    expect(rows.length).toBeGreaterThan(0);
    // Neither section's name holds either word, so every row holds both.
    for (const r of rows) expect(r.toLowerCase(), r).toMatch(/^(?=.*due)(?=.*date)/);
    expect(rows.some((r) => r.endsWith("Set the due date"))).toBe(true);
    expect(rows.some((r) => /\^<date>\s+due date/.test(r))).toBe(true);
    expect(rows.some((r) => r.includes("Next row"))).toBe(false);
    // Every row sits under its section's name, and a section with no row is left out.
    const sections = lines.filter((l) => l.kind === "section").map((l) => l.text);
    expect(sections).toEqual(["Items", "Capture syntax"]);
    expect(lines[0]).toEqual({ kind: "title", text: `Help /DUE date · ${rows.length} rows · j/k scroll · esc close` });
  });

  it("matches a section's name, so that a word like dates shows the whole sheet", () => {
    const rows = helpLines(BINDINGS, "dates").filter((l) => l.kind === "row");
    const all = helpLines().map((l) => l.text);
    const sheet = all.slice(all.indexOf("Dates") + 1);
    for (const r of sheet) expect(rows.map((l) => l.text)).toContain(r);
  });

  it("finds keys as well as words, and says when nothing matches", () => {
    expect(helpLines(BINDINGS, "ctrl-r").filter((l) => l.kind === "row").map((l) => l.text)).toEqual([expect.stringMatching(/ctrl-r\s+Redo/)]);
    expect(helpLines(BINDINGS, "zzqx")).toEqual([{ kind: "title", text: "Help /zzqx · no row matches · esc close" }]);
    // Blank text narrows nothing.
    expect(helpLines(BINDINGS, "   ")).toEqual(helpLines());
    expect(helpLines()[0]?.text).toBe("Help · j/k scroll · / search · esc close");
  });

  it("cuts a row into runs, the words it found marked, in any case", () => {
    const words = searchWords("due D");
    expect(words).toEqual(["due", "d"]);
    expect(matchRuns("  ^ Set the Due date", words)).toEqual([
      { text: "  ^ Set the ", match: false },
      { text: "Due", match: true },
      { text: " ", match: false },
      { text: "d", match: true },
      { text: "ate", match: false },
    ]);
    // Regex characters are only text.
    expect(matchRuns("a (b) c", searchWords("(b)"))).toEqual([
      { text: "a ", match: false },
      { text: "(b)", match: true },
      { text: " c", match: false },
    ]);
    expect(matchRuns("plain", [])).toEqual([{ text: "plain", match: false }]);
  });

  it("lights every occurrence of every word, overlapping ones too", () => {
    expect(matchRuns("  j ↓  Move down", searchWords("do own"))).toEqual([
      { text: "  j ↓  Move ", match: false },
      { text: "down", match: true },
    ]);
    expect(matchRuns("aaab", ["aa"])).toEqual([
      { text: "aaa", match: true },
      { text: "b", match: false },
    ]);
  });

  it("names no keys in the title while the search line is open: they are text there", () => {
    const rows = helpLines(BINDINGS, "due").filter((l) => l.kind === "row").length;
    expect(helpLines(BINDINGS, "due", true)[0]?.text).toBe(`Help /due · ${rows} rows`);
    expect(helpLines(BINDINGS, "zzqx", true)[0]?.text).toBe("Help /zzqx · no row matches");
    expect(helpLines(BINDINGS, "", true)[0]?.text).toBe("Help /");
    // The same rows either way.
    expect(helpLines(BINDINGS, "due", true).slice(1)).toEqual(helpLines(BINDINGS, "due").slice(1));
  });
});
