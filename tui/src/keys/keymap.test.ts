import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { BINDINGS, bindingModes, sequences } from "./keymap.js";
import { CTRL_ALIASES, NAMED_TOKENS, isChar, tokenize } from "./tokenize.js";

const vocabulary = JSON.parse(
  readFileSync(fileURLToPath(new URL("../../../crates/todo-daemon/src/keymap.json", import.meta.url)), "utf8"),
) as { actions: Record<string, string[]>; named_tokens: string[]; ctrl_aliases: Record<string, string> };

describe("the daemon's copy of the keymap vocabulary", () => {
  it("lists every action id, by mode", () => {
    const byMode: Record<string, string[]> = { normal: [], help: [], report: [] };
    for (const b of BINDINGS) for (const m of bindingModes(b)) byMode[m]!.push(b.id);
    expect(vocabulary.actions).toEqual(byMode);
  });

  it("lists every named token, and the bindings use only those, characters and ctrl+<letter>", () => {
    expect(vocabulary.named_tokens).toEqual(NAMED_TOKENS);
    for (const b of BINDINGS)
      for (const seq of sequences(b))
        for (const t of seq) expect(isChar(t) || NAMED_TOKENS.includes(t) || /^ctrl\+[a-z]$/.test(t), `${b.id}: ${t}`).toBe(true);
  });

  it("lists the ctrl+<letter> keys that arrive as another key", () => {
    expect(vocabulary.ctrl_aliases).toEqual(CTRL_ALIASES);
    // The bytes those keys send, as Ink reports them.
    expect(tokenize("", { backspace: true })).toEqual(["backspace"]);
    expect(tokenize("", { tab: true })).toEqual(["tab"]);
    expect(tokenize("\n", {})).toEqual(["enter"]);
    expect(tokenize("\r", { return: true })).toEqual(["enter"]);
    expect(tokenize(" ", { ctrl: true })).toEqual(["ctrl+space"]);
  });
});
