// The quick-add preview (Decision 7): ParseCapture's tokens as spans of the
// typed text, and its draft as one summary line. The daemon gives token
// offsets in code points; JS strings index UTF-16 units, so an emoji
// before a token would shift every span after it without the conversion.

import { CaptureTokenKind, type CaptureDraft, type CaptureToken, Priority, Status } from "@todo-tui/client";
import { type LineState, lineWindow } from "../keys/line-editor.js";
import { dayLabel, dayOf } from "./dates.js";
import { personName } from "./item.js";
import { sanitize, singleLine } from "./text.js";

/** A token's place in the text, as JS string indexes (end exclusive). */
export interface Span {
  readonly start: number;
  readonly end: number;
  readonly kind: CaptureTokenKind;
  readonly applied: boolean;
  readonly warning: string;
}

/** The JS string index of each code point of `text`, and the text's length after the last. */
export function codePointIndexes(text: string): number[] {
  const out: number[] = [];
  let at = 0;
  for (const cp of text) {
    out.push(at);
    at += cp.length;
  }
  out.push(at);
  return out;
}

/** The tokens of a draft parsed from `text`, as spans of `text`; a token past its end is dropped. */
export function tokenSpans(text: string, tokens: readonly CaptureToken[]): Span[] {
  const at = codePointIndexes(text);
  const out: Span[] = [];
  for (const t of tokens) {
    const start = at[t.start];
    const end = at[t.end];
    if (start === undefined || end === undefined || end < start) continue;
    out.push({ start, end, kind: t.kind, applied: t.applied, warning: t.warning });
  }
  return out;
}

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
    default:
      return "blue";
  }
}

const priorities: Partial<Record<Priority, string>> = {
  [Priority.HIGHEST]: "+1",
  [Priority.HIGH]: "+2",
  [Priority.MEDIUM]: "+3",
  [Priority.LOW]: "+4",
  [Priority.LOWEST]: "+5",
};

/** `→ title · +2 · ^Fri Oct 30 · #a #b · ⧗dana · >next`: what enter would capture. */
export function captureSummary(draft: CaptureDraft, today: number): string {
  const parts = [`→ ${singleLine(draft.title)}`];
  const priority = priorities[draft.priority];
  if (priority) parts.push(priority);
  if (draft.dueDate) parts.push(`^${dayLabel(dayOf(draft.dueDate), today)}`);
  if (draft.labels.length) parts.push(draft.labels.map((l) => `#${singleLine(l)}`).join(" "));
  const waiting = personName(draft.waitingOn);
  if (waiting) parts.push(`⧗${waiting}`);
  if (draft.status === Status.NEXT) parts.push(">next");
  if (draft.status === Status.SOMEDAY) parts.push(">someday");
  return parts.join(" · ");
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
