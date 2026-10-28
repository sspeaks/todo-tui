// A one-line text editor as a pure reducer (Decision 7): no dependency, and
// testable without Ink. Text arrives as key tokens, so a burst like
// "milk\rbread\r" edits and submits in order.

import { displayWidth, sanitize } from "../format/text.js";
import { altKey, isChar, type KeyToken } from "./tokenize.js";

export interface LineState {
  readonly text: string;
  /** In code points, 0 to the text's length. */
  readonly cursor: number;
}

export const emptyLine: LineState = { text: "", cursor: 0 };

/** `text` with the cursor at its end. */
export const lineOf = (text: string): LineState => ({ text, cursor: [...text].length });

const at = (cps: readonly string[], cursor: number, text: string = cps.join("")): LineState => ({ text, cursor });

/** Inserts text (a typed character or a paste) at the cursor. */
export function insert(line: LineState, text: string): LineState {
  const cps = [...line.text];
  const added = [...text];
  cps.splice(line.cursor, 0, ...added);
  return at(cps, line.cursor + added.length);
}

const isSpace = (c: string | undefined) => c !== undefined && /\s/u.test(c);

/** Where ctrl-w deletes back to: over spaces, then over the word before them. */
function wordStart(cps: readonly string[], cursor: number): number {
  let i = cursor;
  while (i > 0 && isSpace(cps[i - 1])) i--;
  while (i > 0 && !isSpace(cps[i - 1])) i--;
  return i;
}

/** Where a word-forward motion stops: over spaces, then over the word after them. */
function wordEnd(cps: readonly string[], cursor: number): number {
  let i = cursor;
  while (i < cps.length && isSpace(cps[i])) i++;
  while (i < cps.length && !isSpace(cps[i])) i++;
  return i;
}

/** The word keys: alt (or ctrl) with an arrow, readline's alt-b, alt-f and alt-d, and alt-backspace. */
const WORD_KEYS: ReadonlySet<KeyToken> = new Set([
  "alt+left",
  "ctrl+left",
  "alt+b",
  "alt+right",
  "ctrl+right",
  "alt+f",
  "alt+backspace",
  "alt+delete",
  "alt+d",
]);

/**
 * What an alt+X token does in a line being edited: a word key edits; any
 * other alt with a character is ESC then that character, typed within
 * Ink's 20 ms (hard case 3); alt with any other named key (Alt+Up) does
 * nothing, as an esc would throw the line away.
 */
export function altInLine(key: KeyToken): "edit" | "esc" | "ignore" | undefined {
  const alt = altKey(key);
  if (alt === undefined) return undefined;
  if (WORD_KEYS.has(key)) return "edit";
  return isChar(alt) ? "esc" : "ignore";
}

/**
 * The line after one key, or undefined when the key does not edit text
 * (enter, esc, tab, up, …), for the mode to handle.
 */
export function editLine(line: LineState, key: KeyToken): LineState | undefined {
  if (isChar(key)) return insert(line, key === "space" ? " " : key);
  const cps = [...line.text];
  const { cursor } = line;
  switch (key) {
    case "backspace":
      if (cursor === 0) return line;
      cps.splice(cursor - 1, 1);
      return at(cps, cursor - 1);
    case "delete":
    case "ctrl+d":
      if (cursor === cps.length) return line;
      cps.splice(cursor, 1);
      return at(cps, cursor);
    case "left":
    case "ctrl+b":
      return at(cps, Math.max(0, cursor - 1), line.text);
    case "right":
    case "ctrl+f":
      return at(cps, Math.min(cps.length, cursor + 1), line.text);
    case "home":
    case "ctrl+a":
      return at(cps, 0, line.text);
    case "end":
    case "ctrl+e":
      return at(cps, cps.length, line.text);
    case "alt+left":
    case "ctrl+left":
    case "alt+b":
      return at(cps, wordStart(cps, cursor), line.text);
    case "alt+right":
    case "ctrl+right":
    case "alt+f":
      return at(cps, wordEnd(cps, cursor), line.text);
    case "ctrl+w":
    case "alt+backspace": {
      const start = wordStart(cps, cursor);
      cps.splice(start, cursor - start);
      return at(cps, start);
    }
    case "alt+delete":
    case "alt+d":
      cps.splice(cursor, wordEnd(cps, cursor) - cursor);
      return at(cps, cursor);
    case "ctrl+u":
      return at(cps.slice(cursor), 0);
    case "ctrl+k":
      return at(cps.slice(0, cursor), cursor);
  }
  return undefined;
}

/** The cells a code point takes as drawn (sanitized): two for CJK and most emoji, none for a control character. */
const cellsOf = (cp: string): number => displayWidth(sanitize(cp));

/**
 * The code points a field `width` cells wide shows, [start, end), scrolled
 * to keep the cursor in view. The cursor takes a cell of its own, also at
 * the end. Widths are display cells, so wide characters scroll right.
 */
export function lineWindow(line: LineState, width: number): { start: number; end: number } {
  const w = Math.max(1, width);
  const cells = [...line.text].map(cellsOf);
  const at = (i: number) => (i === line.cursor ? Math.max(1, cells[i] ?? 1) : (cells[i] ?? 0));
  // The cursor's cell, then as much before it as fits.
  let start = line.cursor;
  let used = at(start);
  while (start > 0 && used + at(start - 1) <= w) used += at(--start);
  // From there, as much as fits.
  let end = start;
  for (let total = 0; end < cells.length && total + at(end) <= w; end++) total += at(end);
  return { start, end };
}

/**
 * The part of the line a field `width` cells wide shows, scrolled to keep
 * the cursor in view; `under` is the character under the cursor (a space at
 * the end).
 */
export function visibleLine(line: LineState, width: number): { before: string; under: string; after: string } {
  const cps = [...line.text];
  const { start, end } = lineWindow(line, width);
  return {
    before: cps.slice(start, line.cursor).join(""),
    under: cps[line.cursor] ?? " ",
    after: cps.slice(line.cursor + 1, Math.max(line.cursor + 1, end)).join(""),
  };
}
