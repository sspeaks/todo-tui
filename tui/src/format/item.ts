// How an Item reads in a list row and in the detail pane (design 2.2): the
// cells from @todo-tui/present, fitted to terminal columns here.

import type { RowCells } from "@todo-tui/present/item";
import { displayWidth, fit, truncate } from "./text.js";

export * from "@todo-tui/present/item";

/** Between a title and its parent. */
const PARENT_MARK = "  ↳ ";
/** The fewest cells of a parent's title worth showing; with less room it is left out. */
export const MIN_PARENT = 4;

/**
 * The title cell in exactly `width` cells: the title, then the parent
 * ("  ↳ Ship auth…") in the room the title leaves, the title never cut for
 * it. `parent` is that suffix, to render dim; empty when it does not fit.
 */
export function titleCell(title: string, parent: string, width: number): { title: string; parent: string } {
  const room = width - displayWidth(title) - displayWidth(PARENT_MARK);
  if (!parent || room < MIN_PARENT) return { title: fit(title, width), parent: "" };
  return { title, parent: fit(PARENT_MARK + truncate(parent, room), width - displayWidth(title)) };
}

/** The waiting cell in exactly `width` cells: a long name cut, the age kept ("⧗the acco… 3d"). */
export function fitWaiting(cells: Pick<RowCells, "waiting" | "waitingAge">, width: number): string {
  const { waiting, waitingAge: age } = cells;
  if (!age || displayWidth(waiting) <= width) return fit(waiting, width);
  return fit(`${truncate(waiting.slice(0, -age.length), width - displayWidth(age))}${age}`, width);
}
