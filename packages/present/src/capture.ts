// The quick-add preview (Decision 7): ParseCapture's tokens as spans of the
// typed text, and its draft as one summary line. The daemon gives token
// offsets in code points; JS strings index UTF-16 units, so an emoji
// before a token would shift every span after it without the conversion.

import { type CaptureDraft, type CaptureToken, type CaptureTokenKind, Priority, Status } from "@todo-tui/client/portable";
import { dayLabel, dayOf } from "./dates.js";
import { personName } from "./item.js";
import { describeRule } from "./recurrence.js";
import { singleLine } from "./text.js";

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

const priorities: Partial<Record<Priority, string>> = {
  [Priority.HIGHEST]: "+1",
  [Priority.HIGH]: "+2",
  [Priority.MEDIUM]: "+3",
  [Priority.LOW]: "+4",
  [Priority.LOWEST]: "+5",
};

/**
 * `→ title · +2 · ^Fri Oct 30 · repeats every Fri · #a #b · ⧗dana · >next`:
 * what enter would capture. A rule without a ^ date is due on its first
 * date, as the capture stores it; one whose first date is not the due date
 * says when it is.
 */
export function captureSummary(draft: CaptureDraft, today: number): string {
  const parts = [`→ ${singleLine(draft.title)}`];
  const priority = priorities[draft.priority];
  if (priority) parts.push(priority);
  const due = draft.dueDate ?? draft.recurrenceFirst;
  if (due) parts.push(`^${dayLabel(dayOf(due), today)}`);
  if (draft.recurrence) {
    const first = draft.recurrenceFirst;
    parts.push(`repeats ${describeRule(draft.recurrence)}`);
    if (first && due && dayOf(first) !== dayOf(due)) parts.push(`first ${dayLabel(dayOf(first), today)}`);
  }
  if (draft.labels.length) parts.push(draft.labels.map((l) => `#${singleLine(l)}`).join(" "));
  const waiting = personName(draft.waitingOn);
  if (waiting) parts.push(`⧗${waiting}`);
  if (draft.status === Status.NEXT) parts.push(">next");
  if (draft.status === Status.SOMEDAY) parts.push(">someday");
  return parts.join(" · ");
}
