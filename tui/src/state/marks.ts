// Marks (Decision 8, m4_arch §2.3), as pure functions over ids: m toggles
// one, V marks the rows between its anchor and the selection, and marks
// whose Item left the tab are dropped.

import type { Visual } from "./types.js";

/** The marks with `id` toggled. */
export function toggleMark(marks: ReadonlySet<string>, id: string): ReadonlySet<string> {
  const next = new Set(marks);
  if (!next.delete(id)) next.add(id);
  return next;
}

/** V's anchor index in `ids`: where its Item is, else where it was. */
export const anchorIndex = (visual: Visual, ids: readonly string[]): number => {
  const at = ids.indexOf(visual.anchorId);
  return at >= 0 ? at : Math.min(visual.anchor, ids.length - 1);
};

/** V's marks with the selection at `index`: the marks from before V, and every row from the anchor to it. */
export function rangeMarks(visual: Visual, ids: readonly string[], index: number): ReadonlySet<string> {
  const marks = new Set(visual.base);
  if (index < 0) return marks;
  const a = anchorIndex(visual, ids);
  for (let i = Math.max(0, Math.min(a, index)); i <= Math.max(a, index); i++) {
    const id = ids[i];
    if (id !== undefined) marks.add(id);
  }
  return marks;
}

/** `marks` without the ids not in `present`; the same set when none left. */
export function pruneMarks(marks: ReadonlySet<string>, present: ReadonlySet<string>): ReadonlySet<string> {
  let gone = false;
  for (const id of marks) if (!present.has(id)) gone = true;
  return gone ? new Set([...marks].filter((id) => present.has(id))) : marks;
}

/** The marked ids in list order. */
export const markedInOrder = (marks: ReadonlySet<string>, ids: readonly string[]): string[] => (marks.size ? ids.filter((id) => marks.has(id)) : []);
