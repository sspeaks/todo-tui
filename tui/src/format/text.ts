// User text on its way to the terminal: sanitize and singleLine (from
// @todo-tui/present), and the display-width arithmetic the list's columns
// need.

import { sanitize, singleLine } from "@todo-tui/present/text";

export { sanitize, singleLine };

const segmenter = new Intl.Segmenter();
const VISIBLE = /[^\p{Default_Ignorable_Code_Point}\p{Control}\p{Format}\p{Nonspacing_Mark}\p{Enclosing_Mark}\p{Surrogate}]/v;
const EMOJI = /^\p{RGI_Emoji}$/v;
const PICTOGRAPHIC = /\p{Extended_Pictographic}/gu;

// East Asian Wide and Fullwidth blocks, enough for CJK, Hangul and the
// emoji blocks; a miss only misaligns a column, since Ink truncates the line.
const WIDE: readonly [number, number][] = [
  [0x1100, 0x115f],
  [0x2e80, 0x303e],
  [0x3041, 0x33ff],
  [0x3400, 0x4dbf],
  [0x4e00, 0x9fff],
  [0xa000, 0xa4cf],
  [0xa960, 0xa97f],
  [0xac00, 0xd7a3],
  [0xf900, 0xfaff],
  [0xfe10, 0xfe19],
  [0xfe30, 0xfe6f],
  [0xff00, 0xff60],
  [0xffe0, 0xffe6],
  [0x1f300, 0x1f64f],
  [0x1f900, 0x1f9ff],
  [0x20000, 0x2fffd],
  [0x30000, 0x3fffd],
];

function graphemeWidth(g: string): number {
  const at = g.search(VISIBLE);
  if (at < 0) return 0;
  if (EMOJI.test(g) || (g.includes("‍") && (g.match(PICTOGRAPHIC)?.length ?? 0) >= 2)) return 2;
  const cp = g.codePointAt(at) ?? 0;
  return WIDE.some(([lo, hi]) => cp >= lo && cp <= hi) ? 2 : 1;
}

const graphemes = (text: string): string[] => Array.from(segmenter.segment(text), (s) => s.segment);

/** Terminal cells `text` takes (sanitized text: no escapes or controls). */
export function displayWidth(text: string): number {
  if (/^[ -~]*$/.test(text)) return text.length;
  let width = 0;
  for (const g of graphemes(text)) width += graphemeWidth(g);
  return width;
}

/** `text` cut to `width` cells, ending in … when cut. */
export function truncate(text: string, width: number): string {
  if (width <= 0) return "";
  if (displayWidth(text) <= width) return text;
  let out = "";
  let used = 0;
  for (const g of graphemes(text)) {
    const w = graphemeWidth(g);
    if (used + w > width - 1) break;
    out += g;
    used += w;
  }
  return `${out}…`;
}

/** `text` cut or padded with spaces to exactly `width` cells. */
export function fit(text: string, width: number): string {
  const cut = truncate(text, width);
  return cut + " ".repeat(Math.max(0, width - displayWidth(cut)));
}

/** Word-wraps multi-line text to `width` cells; a word longer than a line is broken. */
export function wrap(text: string, width: number): string[] {
  const out: string[] = [];
  const w = Math.max(1, width);
  for (const para of text.split("\n")) {
    let line = "";
    let used = 0;
    for (const word of para.split(/(\s+)/)) {
      if (word === "") continue;
      const ww = displayWidth(word);
      if (/^\s+$/.test(word)) {
        if (used > 0 && used + ww <= w) {
          line += word;
          used += ww;
        }
        continue;
      }
      if (used + ww > w && used > 0) {
        out.push(line.trimEnd());
        line = "";
        used = 0;
      }
      if (ww <= w) {
        line += word;
        used += ww;
        continue;
      }
      for (const g of graphemes(word)) {
        const gw = graphemeWidth(g);
        if (used + gw > w) {
          out.push(line);
          line = "";
          used = 0;
        }
        line += g;
        used += gw;
      }
    }
    out.push(line.trimEnd());
  }
  return out;
}
