// A one-line text editor as a pure reducer (Decision 7): no dependency, and
// testable without Ink. Text arrives as key tokens, so a burst like
// "milk\rbread\r" edits and submits in order.

import { isChar, type KeyToken } from "./tokenize.js";

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
    case "ctrl+w": {
      const start = wordStart(cps, cursor);
      cps.splice(start, cursor - start);
      return at(cps, start);
    }
    case "ctrl+u":
      return at(cps.slice(cursor), 0);
    case "ctrl+k":
      return at(cps.slice(0, cursor), cursor);
  }
  return undefined;
}

/**
 * The part of the line a field `width` cells wide shows, scrolled to keep
 * the cursor in view; `under` is the character under the cursor (a space at
 * the end). Widths count code points.
 */
export function visibleLine(line: LineState, width: number): { before: string; under: string; after: string } {
  const cps = [...line.text];
  const w = Math.max(1, width);
  // The cursor takes a cell of its own, also at the end.
  const start = Math.max(0, line.cursor - w + 1);
  const end = start + w;
  return {
    before: cps.slice(start, line.cursor).join(""),
    under: cps[line.cursor] ?? " ",
    after: cps.slice(line.cursor + 1, Math.max(line.cursor + 1, end)).join(""),
  };
}
