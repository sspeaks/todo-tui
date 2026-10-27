import { describe, expect, it } from "vitest";
import { BINDINGS, type Binding, bindingModes, isAvailable, sequences } from "./keymap.js";
import { buildTrie, continuations, step } from "./trie.js";

describe("the keymap", () => {
  it("compiles: unique ids, snake_case, no sequence bound twice or both complete and a prefix", () => {
    expect(() => buildTrie(BINDINGS, "normal")).not.toThrow();
    expect(() => buildTrie(BINDINGS, "help")).not.toThrow();
    const ids = BINDINGS.map((b) => b.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const id of ids) expect(id).toMatch(/^[a-z][a-z0-9]*(_[a-z0-9]+)*$/);
    // The invariant, checked directly over every mode's sequences.
    for (const mode of ["normal", "help"] as const) {
      const seqs = BINDINGS.filter((b) => bindingModes(b).includes(mode)).flatMap((b) => sequences(b).map((s) => s.join(" ")));
      for (const a of seqs) for (const b of seqs) if (a !== b) expect(b.startsWith(`${a} `), `${a} is complete and a prefix of ${b}`).toBe(false);
    }
  });

  it("refuses a binding that is both complete and a prefix, or bound twice", () => {
    const g: Binding = { id: "oops", keys: ["g"], action: { type: "top" }, section: "App", help: "" };
    expect(() => buildTrie([...BINDINGS, g], "normal")).toThrow(/also a prefix|extends/);
    const dd: Binding = { id: "d_alone", keys: ["d"], action: { type: "top" }, section: "App", help: "" };
    expect(() => buildTrie([dd, { ...dd, id: "dd", keys: ["d d"] }], "normal")).toThrow(/extends/);
    expect(() => buildTrie([dd, { ...dd, id: "d2" }], "normal")).toThrow(/bound to both/);
    expect(() => buildTrie([dd, dd], "normal")).toThrow(/used twice/);
  });

  it("binds navigation, triage, marks, search, A, E and ctrl-z", () => {
    const available = BINDINGS.filter((b) => bindingModes(b).includes("normal") && isAvailable(b)).map((b) => b.id);
    expect(available).toEqual(
      expect.arrayContaining(["move_down", "move_up", "top", "bottom", "half_page_down", "half_page_up", "view_1", "view_9", "view_prev", "view_next", "help", "command", "quit"]),
    );
    expect(available).toEqual(
      expect.arrayContaining(["mark_done", "mark_cancelled", "set_status", "set_priority_1", "clear_priority", "toggle_archive", "delete", "undo", "redo"]),
    );
    expect(available).toEqual(expect.arrayContaining(["quick_add", "edit_title", "edit_labels", "set_due", "defer", "set_waiting", "clear_waiting"]));
    expect(available).toEqual(expect.arrayContaining(["mark", "visual_mark", "narrow", "quick_add_child", "edit_description", "suspend"]));
    expect(BINDINGS.filter((b) => b.available)).toEqual([]);
  });
});

describe("the trie dispatcher", () => {
  const run = (keys: string[]): string[] => {
    const root = buildTrie(BINDINGS, "normal");
    let pending: string[] = [];
    const out: string[] = [];
    for (const k of keys) {
      const r = step(root, pending, k);
      pending = r.kind === "pending" ? r.pending : [];
      if (r.kind === "run") out.push(r.binding.id);
      if (r.kind === "unbound") out.push(`unbound ${r.keys.join(" ")}`);
    }
    return out;
  };

  it("runs bindings, waits on prefixes and drops unbound continuations (hard case 4)", () => {
    expect(run(["j", "j", "j", "g", "g"])).toEqual(["move_down", "move_down", "move_down", "top"]);
    expect(run(["d", "j"])).toEqual(["unbound d j"]);
    expect(run(["d", "d"])).toEqual(["delete"]);
    expect(run(["p", "3", "p", "9"])).toEqual(["set_priority_3", "unbound p 9"]);
    expect(run(["Z"])).toEqual(["unbound Z"]);
    expect(run(["fkey", "?"])).toEqual(["help", "help"]);
  });

  it("lists a prefix's continuations", () => {
    const root = buildTrie(BINDINGS, "normal");
    expect(continuations(root, ["g"]).map(([k, b]) => [k, b.id])).toEqual([["g", "top"]]);
    expect(continuations(root, ["p"]).map(([k]) => k)).toEqual(["1", "2", "3", "4", "5", "0"]);
  });
});
