// [keymap] overrides (design 8.2) applied to the default bindings. A key
// sequence names the action it runs: it is added to that binding, in each
// of the binding's modes, and taken off whatever held it there. "" unbinds
// it. The daemon has already checked the ids and key names against this
// table's vocabulary (crates/todo-daemon/src/keymap.json); what only the
// table can tell, a sequence that would be both complete and a prefix, is
// checked here.
//
// The overrides apply as one set: every sequence is first taken off what
// held it, then each is bound to its action, so `d = "delete"` with
// `"d d" = ""` moves delete onto d whatever order they are read in. A
// binding that still conflicts with the rest is skipped with a warning; the
// others apply.

import { type Binding, type KeymapMode, bindingModes, sequences } from "./keymap.js";
import { CTRL_ALIASES, NAMED_TOKENS, isChar } from "./tokenize.js";
import { buildTrie } from "./trie.js";

export interface KeymapWarning {
  /** The sequence as written in [keymap]. */
  readonly keys: string;
  readonly message: string;
}

export interface EffectiveKeymap {
  readonly bindings: readonly Binding[];
  readonly warnings: readonly KeymapWarning[];
}

const MODES: readonly KeymapMode[] = ["normal", "help"];

const isToken = (t: string): boolean => (isChar(t) && t !== " ") || NAMED_TOKENS.includes(t) || (/^ctrl\+[a-z]$/.test(t) && !(t in CTRL_ALIASES));

/** `defaults` with `keymap` applied. The result does not depend on the map's order. */
export function applyOverrides(defaults: readonly Binding[], keymap: Readonly<Record<string, string>>): EffectiveKeymap {
  const warnings: KeymapWarning[] = [];
  const binds: { keys: string; target: Binding; modes: readonly KeymapMode[] }[] = [];
  const unbinds: string[] = [];
  for (const keys of Object.keys(keymap).sort()) {
    const action = keymap[keys] ?? "";
    const bad = keys.split(" ").find((t) => !isToken(t));
    if (keys === "" || bad !== undefined) {
      warnings.push({ keys, message: `${JSON.stringify(bad ?? keys)} is not a key` });
      continue;
    }
    if (action === "") {
      unbinds.push(keys);
      continue;
    }
    const target = defaults.find((b) => b.id === action);
    if (!target) {
      warnings.push({ keys, message: `${JSON.stringify(action)} is not an action id` });
      continue;
    }
    binds.push({ keys, target, modes: bindingModes(target) });
  }

  // Take every overridden sequence off what held it: in every mode for an
  // unbind, in the target's modes for a bind.
  const removed = new Map<string, readonly KeymapMode[]>([
    ...unbinds.map((k): [string, readonly KeymapMode[]] => [k, MODES]),
    ...binds.map((b): [string, readonly KeymapMode[]] => [b.keys, b.modes]),
  ]);
  let bindings = defaults.map((b) => {
    const keys = b.keys.filter((k) => !removed.get(k)?.some((m) => bindingModes(b).includes(m)));
    return keys.length === b.keys.length ? b : { ...b, keys };
  });
  for (const keys of unbinds) {
    if (!defaults.some((b) => b.keys.includes(keys))) warnings.push({ keys, message: "is not bound, so there is nothing to unbind" });
  }

  // Then bind each, against everything else already in place.
  for (const { keys, target, modes } of binds) {
    const candidate = bindings.map((b) => (b.id === target.id ? { ...b, keys: [...b.keys, keys] } : b));
    const error = conflict(candidate, modes);
    if (error) {
      warnings.push({ keys, message: error });
      continue;
    }
    bindings = candidate;
  }
  warnings.sort((a, b) => (a.keys < b.keys ? -1 : a.keys > b.keys ? 1 : 0));
  return { bindings: ungroup(defaults, bindings), warnings };
}

/**
 * A help group ("1–9", "p1–p5") one of whose bindings has other keys now no
 * longer says what its keys are: each of them is listed on its own row.
 */
function ungroup(defaults: readonly Binding[], bindings: readonly Binding[]): Binding[] {
  const changed = new Set(bindings.filter((b, i) => b !== defaults[i] && b.helpKeys !== undefined).map((b) => b.helpKeys));
  return bindings.map((b) => {
    if (b.helpKeys === undefined || !changed.has(b.helpKeys)) return b;
    const { helpKeys: _, ...rest } = b;
    return rest;
  });
}

/** Why `bindings` do not compile in one of `modes`, else undefined. */
function conflict(bindings: readonly Binding[], modes: readonly KeymapMode[]): string | undefined {
  for (const mode of modes) {
    try {
      buildTrie(bindings, mode);
    } catch (err) {
      return (err instanceof Error ? err.message : String(err)).replace(/^keymap: /, "");
    }
  }
  return undefined;
}

/** "j/k": the first keys of a down and an up action, or of whichever has one. */
export function keyPair(bindings: readonly Binding[], down: string, up: string, label: (tokens: readonly string[]) => string): string | undefined {
  const keys = [keyFor(bindings, down, label), keyFor(bindings, up, label)].filter((k) => k !== undefined);
  return keys.length ? keys.join("/") : undefined;
}

/** The first key sequence `id` is bound to, as help shows it ("q", "ctrl-c"); undefined when it has none. */
export function keyFor(bindings: readonly Binding[], id: string, label: (tokens: readonly string[]) => string): string | undefined {
  const b = bindings.find((x) => x.id === id);
  const [first] = b ? sequences(b) : [];
  return first && label(first);
}
