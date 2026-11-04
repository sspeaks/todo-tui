// The quick-add line in the terminal: token colours and the runs to draw.
// The spans and the summary line are @todo-tui/present's.

import { CaptureTokenKind } from "@todo-tui/client";
import { type Span, codePointIndexes } from "@todo-tui/present/capture";
import { type LineState, lineWindow } from "../keys/line-editor.js";
import { sanitize } from "./text.js";

export * from "@todo-tui/present/capture";

/** The colour of a token by kind; one that was not applied is red. */
export function tokenColor(span: Pick<Span, "kind" | "applied">): string {
  if (!span.applied) return "red";
  switch (span.kind) {
    case CaptureTokenKind.PRIORITY:
      return "magenta";
    case CaptureTokenKind.DUE_DATE:
      return "cyan";
    case CaptureTokenKind.LABEL:
      return "green";
    case CaptureTokenKind.WAITING:
      return "yellow";
    case CaptureTokenKind.RECURRENCE:
      return "cyanBright";
    default:
      return "blue";
  }
}

/** A run of the quick-add line drawn alike: in a token's colour, or the cursor's cell. */
export interface Segment {
  readonly text: string;
  readonly color?: string;
  readonly cursor?: boolean;
}

/**
 * The quick-add line as runs to draw: the window `width` cells wide around
 * the cursor, each token's text in its kind's colour (`spans` as JS
 * indexes, from tokenSpans), the cursor's cell on its own. Control
 * characters (a pasted ESC) are left out.
 */
export function lineSegments(line: LineState, width: number, spans: readonly Span[]): Segment[] {
  const cps = [...line.text];
  const at = codePointIndexes(line.text);
  const { start, end } = lineWindow(line, width);
  const colorAt = (cp: number) => {
    const js = at[cp] ?? -1;
    const span = spans.find((s) => js >= s.start && js < s.end);
    return span && tokenColor(span);
  };
  const out: Segment[] = [];
  for (let i = start; i < Math.max(end, line.cursor + 1); i++) {
    const cursor = i === line.cursor;
    const text = sanitize(cps[i] ?? " ") || (cursor ? " " : "");
    if (!text) continue;
    const color = colorAt(i);
    const last = out.at(-1);
    if (last && !cursor && !last.cursor && last.color === color) out[out.length - 1] = { ...last, text: last.text + text };
    else out.push({ text, ...(color !== undefined && { color }), ...(cursor && { cursor }) });
  }
  return out;
}
