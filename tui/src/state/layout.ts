// Screen geometry (Decision 3), computed from the terminal size, never
// measured: the frame must never be taller than the terminal, or Ink
// clears and redraws it whole on every frame.

import { displayWidth } from "../format/text.js";
import type { ModeKind } from "./types.js";

export const MIN_COLUMNS = 40;
export const MIN_ROWS = 8;
/** At this width and wider the list and the detail sit side by side. */
export const SPLIT_COLUMNS = 110;
/** The tab bar, the status bar and the message line. */
export const CHROME_ROWS = 3;
/** Quick-add's input and preview, above the list. */
export const QUICK_ADD_ROWS = 2;
/** A prompt's preview (or conflict) line, above the status bar. */
export const PROMPT_ROWS = 1;

/** The rows a mode takes from the body. */
export const modeRows = (mode: ModeKind | undefined): number =>
  mode === "quickAdd" ? QUICK_ADD_ROWS : mode === "prompt" ? PROMPT_ROWS : 0;

export interface Layout {
  readonly columns: number;
  readonly rows: number;
  /** Below MIN_COLUMNS × MIN_ROWS only "Terminal too small" is shown. */
  readonly tooSmall: boolean;
  readonly wide: boolean;
  /** The rows for the list and the detail, between the tab bar (and quick-add) and the status bar (and a prompt's preview). */
  readonly bodyHeight: number;
  readonly listWidth: number;
  /** The detail pane's width: about 40% beside the list, or the whole width as an overlay. */
  readonly detailWidth: number;
}

/** The screen for a terminal size, with the rows `mode` takes left out of the body. */
export function layout(columns: number, rows: number, mode?: ModeKind): Layout {
  const wide = columns >= SPLIT_COLUMNS;
  const detailWidth = wide ? Math.floor(columns * 0.4) : columns;
  return {
    columns,
    rows,
    tooSmall: columns < MIN_COLUMNS || rows < MIN_ROWS,
    wide,
    bodyHeight: Math.max(1, rows - CHROME_ROWS - modeRows(mode)),
    // One column for the separator.
    listWidth: wide ? columns - detailWidth - 1 : columns,
    detailWidth,
  };
}

/** The cells before a row's title: "› ○ +3* ". */
export const ROW_PREFIX = 8;
/** The narrowest a title gets before other columns are dropped. */
export const MIN_TITLE = 20;
/** Space between columns. */
export const GAP = 2;

/** Widths of a row's columns after the title; 0 for a column not shown. */
export interface RowColumns {
  readonly title: number;
  readonly key: number;
  readonly due: number;
  readonly waiting: number;
  readonly markers: number;
}

type Column = "key" | "due" | "waiting" | "markers";
/** Dropped first to last when the title would get narrower than MIN_TITLE. */
const DROP_ORDER: readonly Column[] = ["markers", "waiting", "key", "due"];

/**
 * Fits a row into `width` cells: each column as wide as its widest visible
 * cell, and the title gets the rest, at least MIN_TITLE while columns can
 * be dropped to make room.
 */
export function rowColumns(width: number, natural: Readonly<Record<Column, number>>): RowColumns {
  const shown: Record<Column, number> = { ...natural };
  const used = () => (Object.values(shown) as number[]).reduce((sum, w) => sum + (w ? w + GAP : 0), 0);
  for (const col of DROP_ORDER) {
    if (width - ROW_PREFIX - used() >= MIN_TITLE) break;
    shown[col] = 0;
  }
  return { ...shown, title: Math.max(0, width - ROW_PREFIX - used()) };
}

export interface TabLabel {
  readonly id: string;
  readonly text: string;
  readonly current: boolean;
}

/** Between tab labels. */
export const TAB_SEPARATOR = " │ ";

/**
 * The tab bar's labels: "1 Inbox 3", the count left out when zero or not
 * counted. When they do not fit, the other tabs shorten to "2 T5" (the
 * initial and the count), the current one staying whole.
 */
export function tabLabels(
  views: readonly { id: string; name: string }[],
  counts: ReadonlyMap<string, number>,
  current: string | undefined,
  width: number,
): TabLabel[] {
  const count = (id: string) => {
    const n = counts.get(id) ?? 0;
    return n > 0 ? String(n) : "";
  };
  const full = views.map((v, i) => {
    const n = count(v.id);
    return { id: v.id, text: `${i + 1} ${v.name}${n ? ` ${n}` : ""}`, current: v.id === current };
  });
  const total = (labels: TabLabel[]) => labels.reduce((sum, l, i) => sum + displayWidth(l.text) + (i ? TAB_SEPARATOR.length : 0), 0);
  if (total(full) <= width) return full;
  return full.map((label, i) => (label.current ? label : { ...label, text: `${i + 1} ${[...(views[i]?.name ?? "")][0] ?? ""}${count(label.id)}` }));
}
