// Ink's input events as key tokens. Ink hands over a whole stdin chunk at
// once: keys typed or repeated fast arrive as one "jjj", "milk\rbread\r"
// as one string with no flags, and ESC then a key within 20 ms as meta+key
// (m4_arch §1.3). The dispatcher needs one token per key, in order.

import type { Key } from "ink";

/**
 * One key: a printable character as itself ("j", "G", "?"), or a name:
 * enter, esc, tab, shift+tab, space, backspace, delete, up, down, left,
 * right, home, end, pageup, pagedown, ctrl+<letter>, and fkey for the keys
 * Ink delivers with no input and no flag (F1–F12, insert).
 */
export type KeyToken = string;

const NAMED: readonly [keyof Key, KeyToken][] = [
  ["upArrow", "up"],
  ["downArrow", "down"],
  ["leftArrow", "left"],
  ["rightArrow", "right"],
  ["pageUp", "pageup"],
  ["pageDown", "pagedown"],
  ["home", "home"],
  ["end", "end"],
  ["return", "enter"],
  ["escape", "esc"],
  ["backspace", "backspace"],
  ["delete", "delete"],
];

/** The token for one character of a burst. */
function charToken(ch: string): KeyToken | undefined {
  switch (ch) {
    case "\r":
    case "\n":
      return "enter";
    case " ":
      return "space";
    case "\t":
      return "tab";
    case "\x7f":
    case "\b":
      return "backspace";
    case "\x1b":
      return "esc";
  }
  const code = ch.codePointAt(0) ?? 0;
  if (code >= 1 && code <= 26) return `ctrl+${String.fromCharCode(code + 96)}`;
  if (code < 0x20 || (code >= 0x7f && code <= 0x9f)) return undefined;
  return ch;
}

/**
 * The keys one useInput call stands for. Pastes never come here: usePaste
 * owns them. Ink reports both ESC then X typed within 20 ms and Alt+X (or
 * a modified key such as Alt+Left, CSI 1;3D) as meta+X, so a meta+X key is
 * "alt+X": the dispatcher reads it as esc then X (hard case 3), except
 * where a text mode takes it as a word edit (line-editor.ts).
 */
export function tokenize(input: string, key: Partial<Key>): KeyToken[] {
  const esc: KeyToken[] = key.meta ? ["esc"] : [];
  if (key.tab) return [...esc, key.shift ? "shift+tab" : "tab"];
  for (const [flag, name] of NAMED) {
    if (!key[flag]) continue;
    if (key.meta && !key.ctrl && name !== "esc") return [`alt+${name}`];
    return [...esc, key.ctrl ? `ctrl+${name}` : name];
  }
  if (key.meta && !key.ctrl && [...input].length === 1) {
    const t = charToken(input);
    if (t !== undefined && isChar(t)) return [`alt+${t}`];
  }
  if (key.ctrl) return input ? [...esc, `ctrl+${input === " " ? "space" : input.toLowerCase()}`] : esc;
  // Ink blanks the function keys and gives them no flag.
  if (input === "") return esc.length ? esc : ["fkey"];
  const tokens = [...esc];
  for (const ch of input) {
    const t = charToken(ch);
    // A run of line breaks in one chunk is one enter: a CRLF, or a blank
    // line in text pasted without bracketed paste, would otherwise close
    // quick-add with the rest of the text still to come.
    if (t === "enter" && tokens.at(-1) === "enter") continue;
    if (t !== undefined) tokens.push(t);
  }
  return tokens;
}

/**
 * Tokens written as text, for tests and scripts: characters as typed
 * ("jj" is two j), names in angle brackets ("<esc>", "<ctrl+d>", "<lt>" for <).
 */
export function parseKeys(spec: string): KeyToken[] {
  const tokens: KeyToken[] = [];
  for (const m of spec.matchAll(/<([^<>\s]+)>|([^<]+)|(<)/gu)) {
    if (m[1] !== undefined) tokens.push(m[1] === "lt" ? "<" : m[1]);
    else for (const ch of m[2] ?? m[3] ?? "") {
      const t = charToken(ch);
      if (t !== undefined) tokens.push(t);
    }
  }
  return tokens;
}

/** Whether a token is a character typed into text, rather than a named key. */
export const isChar = (t: KeyToken): boolean => t === "space" || [...t].length === 1;

/** The key of an "alt+X" token (X), else undefined. */
export const altKey = (t: KeyToken): KeyToken | undefined => (t.startsWith("alt+") && t.length > 4 ? t.slice(4) : undefined);

/** How help and hints show a token: "ctrl-d", "↓", "F1". */
export function keyLabel(t: KeyToken): string {
  switch (t) {
    case "up":
      return "↑";
    case "down":
      return "↓";
    case "left":
      return "←";
    case "right":
      return "→";
    case "fkey":
      return "F1";
    case "pageup":
      return "pgup";
    case "pagedown":
      return "pgdn";
  }
  return t.replace(/^ctrl\+/, "ctrl-").replace(/^shift\+/, "shift-").replace(/^alt\+/, "alt-");
}

/** A key sequence as help shows it: "gg", "p 1", "ctrl-d". */
export function sequenceLabel(tokens: readonly KeyToken[]): string {
  const labels = tokens.map(keyLabel);
  return labels.every((l) => [...l].length === 1) ? labels.join("") : labels.join(" ");
}
