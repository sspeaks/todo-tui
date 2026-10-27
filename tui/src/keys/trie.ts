// A keymap mode compiled to a trie. There are no timeouts: no binding is
// both complete and a prefix (buildTrie refuses one), so every key either
// runs a binding, waits for more, or is dropped.

import { type Binding, type KeymapMode, bindingModes, sequences } from "./keymap.js";
import type { KeyToken } from "./tokenize.js";

export interface TrieNode {
  readonly binding?: Binding;
  readonly next: ReadonlyMap<KeyToken, TrieNode>;
}

interface MutableNode {
  binding?: Binding;
  next: Map<KeyToken, MutableNode>;
}

/** The trie of `mode`'s bindings; throws on a sequence bound twice or bound and also a prefix. */
export function buildTrie(bindings: readonly Binding[], mode: KeymapMode): TrieNode {
  const root: MutableNode = { next: new Map() };
  const ids = new Set<string>();
  for (const b of bindings) {
    if (ids.has(b.id)) throw new Error(`keymap: id ${b.id} is used twice`);
    ids.add(b.id);
    if (!bindingModes(b).includes(mode)) continue;
    for (const keys of sequences(b)) {
      const label = keys.join(" ");
      let node = root;
      for (const k of keys) {
        if (node.binding) throw new Error(`keymap: ${label} (${b.id}) extends ${node.binding.id}, which is complete`);
        let child = node.next.get(k);
        if (!child) node.next.set(k, (child = { next: new Map() }));
        node = child;
      }
      if (node.binding) throw new Error(`keymap: ${label} is bound to both ${node.binding.id} and ${b.id}`);
      if (node.next.size) throw new Error(`keymap: ${label} (${b.id}) is also a prefix`);
      node.binding = b;
    }
  }
  return root;
}

export type StepResult =
  /** Waiting for more keys. */
  | { kind: "pending"; pending: KeyToken[] }
  | { kind: "run"; binding: Binding }
  /** Bound to nothing, alone or after the pending keys: the keys are dropped. */
  | { kind: "unbound"; keys: KeyToken[] };

/** Feeds one key after the `pending` ones. */
export function step(root: TrieNode, pending: readonly KeyToken[], key: KeyToken): StepResult {
  let node: TrieNode | undefined = root;
  for (const k of pending) node = node?.next.get(k);
  const child = node?.next.get(key);
  if (!child) return { kind: "unbound", keys: [...pending, key] };
  if (child.binding) return { kind: "run", binding: child.binding };
  return { kind: "pending", pending: [...pending, key] };
}

/** The bindings that complete `pending`, with the key after it, for the which-key hint. */
export function continuations(root: TrieNode, pending: readonly KeyToken[]): [KeyToken, Binding][] {
  let node: TrieNode | undefined = root;
  for (const k of pending) node = node?.next.get(k);
  const out: [KeyToken, Binding][] = [];
  for (const [k, child] of node?.next ?? []) if (child.binding) out.push([k, child.binding]);
  return out;
}
