// User text made safe to show on one line: escape sequences and control
// characters stripped (a title can hold ESC, and Ink passes SGR codes
// through).

// CSI (ESC [ … final byte), the string controls OSC, DCS, SOS, PM and APC
// up to BEL or ST, other ESC sequences, a lone ESC, and the 8-bit forms of CSI
// and the string controls.
const ESCAPES =
  /\x1b\[[0-?]*[ -/]*[@-~]?|\x1b[\]PX^_][^\x07\x1b]*(?:\x07|\x1b\\)?|\x1b[ -/]*[0-~]?|\x9b[0-?]*[ -/]*[@-~]?|[\x90\x98\x9d\x9e\x9f][^\x07\x1b\x9c]*(?:\x07|\x1b\\|\x9c)?/g;
// C0 (but tab and newline), DEL and C1; and the bidi embeddings, overrides
// and isolates, which reorder what comes after them on the line.
const CONTROLS = /[\x00-\x08\x0b-\x1f\x7f-\x9f‪-‮⁦-⁩]/g;

/**
 * User text safe to render: escape sequences and control characters removed,
 * tabs as spaces. Line breaks are kept (as \n) only when `multiline`.
 */
export function sanitize(text: string, { multiline = false } = {}): string {
  const lines = text.replace(ESCAPES, "").replace(/\r\n?/g, "\n").replace(/\t/g, " ");
  return (multiline ? lines : lines.replace(/\n/g, " ")).replace(CONTROLS, "");
}

/** Sanitized, with whitespace collapsed, so it always renders on one line. */
export const singleLine = (text: string): string => sanitize(text).replace(/\s+/g, " ").trim();
