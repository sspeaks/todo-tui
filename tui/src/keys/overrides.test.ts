import { describe, expect, it } from "vitest";
import { helpLines } from "./help.js";
import { BINDINGS, type Binding } from "./keymap.js";
import { applyOverrides } from "./overrides.js";
import { buildTrie, step } from "./trie.js";

const keysOf = (bindings: readonly Binding[], id: string) => bindings.find((b) => b.id === id)?.keys;

/** What `keys` runs in `mode` under `bindings`. */
function runs(bindings: readonly Binding[], keys: string[], mode: "normal" | "help" = "normal"): string {
  const root = buildTrie(bindings, mode);
  let pending: string[] = [];
  for (const k of keys) {
    const r = step(root, pending, k);
    if (r.kind === "run") return r.binding.id;
    if (r.kind === "unbound") return "unbound";
    pending = r.pending;
  }
  return "pending";
}

describe("[keymap] overrides", () => {
  it("adds a key to an action, which keeps its own", () => {
    const { bindings, warnings } = applyOverrides(BINDINGS, { K: "move_up" });
    expect(warnings).toEqual([]);
    expect(keysOf(bindings, "move_up")).toEqual(["k", "up", "K"]);
    expect(runs(bindings, ["K"])).toBe("move_up");
    expect(runs(bindings, ["k"])).toBe("move_up");
  });

  it("moves a key from one action to another", () => {
    const { bindings } = applyOverrides(BINDINGS, { x: "mark_cancelled" });
    expect(runs(bindings, ["x"])).toBe("mark_cancelled");
    expect(keysOf(bindings, "mark_done")).toEqual([]);
    // A sequence of several keys, under a prefix of its own.
    const { bindings: b2, warnings } = applyOverrides(BINDINGS, { "g d": "mark_done" });
    expect(warnings).toEqual([]);
    expect(runs(b2, ["g", "d"])).toBe("mark_done");
    expect(runs(b2, ["g", "g"])).toBe("top");
  });

  it("unbinds a key with an empty action", () => {
    const { bindings, warnings } = applyOverrides(BINDINGS, { q: "" });
    expect(warnings).toEqual([]);
    expect(runs(bindings, ["q"])).toBe("unbound");
    expect(runs(bindings, ["ctrl+c"])).toBe("quit");
    // In help too: q closed it.
    expect(runs(bindings, ["q"], "help")).toBe("unbound");
    expect(runs(bindings, ["esc"], "help")).toBe("help_close");
  });

  it("unbinds h and l, keeping [ and ]", () => {
    const { bindings, warnings } = applyOverrides(BINDINGS, { h: "", l: "" });
    expect(warnings).toEqual([]);
    expect(runs(bindings, ["h"])).toBe("unbound");
    expect(runs(bindings, ["l"])).toBe("unbound");
    expect(runs(bindings, ["["])).toBe("view_prev");
    expect(runs(bindings, ["]"])).toBe("view_next");
  });

  it("skips an override that breaks the keymap, keeping the others", () => {
    const { bindings, warnings } = applyOverrides(BINDINGS, { p: "quit", K: "move_up", "d d d": "quit" });
    expect(warnings).toEqual([
      { keys: "d d d", message: "d d d (quit) extends delete, which is complete" },
      { keys: "p", message: "p (quit) is also a prefix" },
    ]);
    expect(runs(bindings, ["p", "1"])).toBe("set_priority_1");
    expect(runs(bindings, ["K"])).toBe("move_up");
    expect(keysOf(bindings, "quit")).toEqual(["q", "ctrl+c"]);
  });

  it("warns about what the daemon would have refused, and an unbind of nothing", () => {
    const { bindings, warnings } = applyOverrides(BINDINGS, { K: "set_priority", "<f2>": "help", "ctrl+i": "quit", Z: "" });
    expect(warnings.map((w) => [w.keys, w.message])).toEqual([
      ["<f2>", '"<f2>" is not a key'],
      ["K", '"set_priority" is not an action id'],
      ["Z", "is not bound, so there is nothing to unbind"],
      ["ctrl+i", '"ctrl+i" is not a key'],
    ]);
    expect(bindings).toEqual(BINDINGS);
  });

  it("applies the overrides as one set: a move onto its own prefix works in any order", () => {
    for (const keymap of [{ d: "delete", "d d": "" }, { "d d": "", d: "delete" }]) {
      const { bindings, warnings } = applyOverrides(BINDINGS, keymap);
      expect(warnings).toEqual([]);
      expect(keysOf(bindings, "delete")).toEqual(["d"]);
      expect(runs(bindings, ["d"])).toBe("delete");
    }
    const { bindings, warnings } = applyOverrides(BINDINGS, { g: "top", "g g": "" });
    expect(warnings).toEqual([]);
    expect(runs(bindings, ["g"])).toBe("top");
    expect(runs(bindings, ["g"], "help")).toBe("unbound");
    expect(runs(bindings, ["home"], "help")).toBe("help_top");
  });

  it("binds a help-mode action in help only", () => {
    const { bindings, warnings } = applyOverrides(BINDINGS, { x: "help_close" });
    expect(warnings).toEqual([]);
    expect(runs(bindings, ["x"], "help")).toBe("help_close");
    expect(runs(bindings, ["x"])).toBe("mark_done");
  });

  it("applies in sorted key order, never mutating the defaults", () => {
    const before = JSON.stringify(BINDINGS.map((b) => b.keys));
    const a = applyOverrides(BINDINGS, { b: "mark_done", a: "mark_done" });
    const b = applyOverrides(BINDINGS, { a: "mark_done", b: "mark_done" });
    expect(keysOf(a.bindings, "mark_done")).toEqual(["x", "a", "b"]);
    expect(a).toEqual(b);
    expect(JSON.stringify(BINDINGS.map((b) => b.keys))).toBe(before);
    expect(applyOverrides(BINDINGS, {}).bindings).toEqual(BINDINGS);
  });

  it("lists the effective keys in help, splitting a group whose keys changed", () => {
    const { bindings } = applyOverrides(BINDINGS, { K: "move_up", "p 6": "set_priority_1", x: "" });
    const text = helpLines(bindings).map((l) => l.text);
    expect(text.find((t) => t.endsWith("Previous row (detail focused: scroll)"))).toMatch(/^ {2}k ↑ K /);
    expect(text.find((t) => t.includes("p1 p6"))).toMatch(/Set the priority$/);
    expect(text.find((t) => t.includes("p1–p5"))).toBeUndefined();
    expect(text.some((t) => t.endsWith("Mark done"))).toBe(false);
  });
});
