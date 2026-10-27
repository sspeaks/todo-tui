// Selection and scrolling over a server-ordered list (Decision 3,
// m4_arch §2.1), as pure functions over the list's ids. The selection is an
// id; the scroll is anchored on the top row's id, so rows inserted above
// move neither the screen nor the selection.

export interface ListState {
  readonly selectedId?: string;
  /** The selection's index at the last reconcile: where it falls back to when its Item leaves. */
  readonly anchor: number;
  /** The first visible row. */
  readonly topId?: string;
  readonly offset: number;
}

export const initialList: ListState = { anchor: 0, offset: 0 };

/** Rows kept visible above and below the selection. */
export const SCROLLOFF = 2;

/** A ListState settled against the current ids, with the selection's index (-1 when empty). */
export interface Resolved extends ListState {
  readonly index: number;
}

const clamp = (n: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, n));

/** Clamps `offset` so that row `index` sits at least SCROLLOFF rows inside a `height`-row window. */
function scrollTo(offset: number, index: number, length: number, height: number): number {
  const so = Math.min(SCROLLOFF, Math.floor((height - 1) / 2));
  const kept = clamp(offset, index - height + 1 + so, index - so);
  return clamp(kept, 0, Math.max(0, length - height));
}

function settle(index: number, offset: number, ids: readonly string[], height: number): Resolved {
  if (!ids.length) return { anchor: 0, offset: 0, index: -1 };
  const i = clamp(index, 0, ids.length - 1);
  const top = scrollTo(offset, i, ids.length, Math.max(1, height));
  return { selectedId: ids[i], anchor: i, topId: ids[top], offset: top, index: i };
}

/**
 * The state for `ids` shown `height` rows high: the selected Item where it
 * now is, else the row at its old index (the next one), else the last; the
 * top row where it now is, else the old offset; then scrolled just enough
 * to keep the selection in view.
 */
export function reconcile(list: ListState, ids: readonly string[], height: number): Resolved {
  const found = list.selectedId === undefined ? -1 : ids.indexOf(list.selectedId);
  const top = list.topId === undefined ? -1 : ids.indexOf(list.topId);
  return settle(found >= 0 ? found : list.anchor, top >= 0 ? top : list.offset, ids, height);
}

/** Selects row `index` (clamped). */
export function select(list: ListState, ids: readonly string[], height: number, index: number): Resolved {
  return settle(index, reconcile(list, ids, height).offset, ids, height);
}

/** Moves the selection `by` rows. */
export function move(list: ListState, ids: readonly string[], height: number, by: number): Resolved {
  const r = reconcile(list, ids, height);
  return settle(r.index + by, r.offset, ids, height);
}

/** ctrl-d / ctrl-u: moves the selection and the window by half a page, so the selection keeps its screen line. */
export function halfPage(list: ListState, ids: readonly string[], height: number, dir: 1 | -1): Resolved {
  const r = reconcile(list, ids, height);
  const by = Math.max(1, Math.floor(height / 2)) * dir;
  const offset = clamp(r.offset + by, 0, Math.max(0, ids.length - height));
  return settle(r.index + by, offset, ids, height);
}

/** Whether two states are the same, ignoring the index. */
export const sameList = (a: ListState, b: ListState): boolean =>
  a.selectedId === b.selectedId && a.anchor === b.anchor && a.topId === b.topId && a.offset === b.offset;

/** The ListState part of a Resolved. */
export const listOf = ({ selectedId, anchor, topId, offset }: Resolved): ListState => ({
  anchor,
  offset,
  ...(selectedId !== undefined && { selectedId }),
  ...(topId !== undefined && { topId }),
});
