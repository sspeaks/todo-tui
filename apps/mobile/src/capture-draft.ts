// The quick-add buttons' choices as capture tokens (design 2.3), appended
// to the title so the daemon parses one text as for any capture: one op,
// one Undo, and the same preview.
import { Priority } from "@todo-tui/client/portable";
import { isoDay } from "@todo-tui/present";

export interface CaptureChoices {
  /** Due day number. */
  due?: number;
  priority?: Priority;
  labels: readonly string[];
  waiting?: string;
  /** Instead of the default inbox. */
  status?: "next" | "someday";
  /** A repeat rule as ParseRecurrence takes it, without the `@`: "daily", "mon,thu", "2w". */
  repeat?: string;
}

export const NO_CHOICES: CaptureChoices = { labels: [] };

const PRIORITY_TOKEN: Partial<Record<Priority, string>> = {
  [Priority.HIGHEST]: "+1",
  [Priority.HIGH]: "+2",
  [Priority.MEDIUM]: "+3",
  [Priority.LOW]: "+4",
  [Priority.LOWEST]: "+5",
};

/** A label as one `#` word: whitespace becomes "-", control characters go. */
export const cleanLabel = (label: string): string =>
  label
    .replace(/[\p{Cc}]/gu, "")
    .trim()
    .replace(/^#+/, "")
    .replace(/\s+/g, "-");

/** A waiting name as a `~` token: quoted when it has spaces, its own quotes dropped. */
export function waitingToken(name: string): string | undefined {
  const clean = name.replace(/[\p{Cc}"]/gu, "").replace(/\s+/g, " ").trim();
  if (!clean) return undefined;
  return /\s/.test(clean) ? `~"${clean}"` : `~${clean}`;
}

export function choiceTokens(c: CaptureChoices): string[] {
  const out: string[] = [];
  const p = c.priority === undefined ? undefined : PRIORITY_TOKEN[c.priority];
  if (p) out.push(p);
  if (c.due !== undefined) out.push(`^${isoDay(c.due)}`);
  for (const l of c.labels) {
    const clean = cleanLabel(l);
    if (clean) out.push(`#${clean}`);
  }
  if (c.waiting !== undefined) {
    const w = waitingToken(c.waiting);
    if (w) out.push(w);
  }
  if (c.status) out.push(`>${c.status}`);
  const rule = c.repeat?.trim().replace(/^@/, "").replace(/\s+/g, "");
  if (rule) out.push(`@${rule}`);
  return out;
}

/** The capture text: what was typed, then the chosen tokens. */
export function captureText(title: string, c: CaptureChoices): string {
  return [title.trim(), ...choiceTokens(c)].filter(Boolean).join(" ");
}

export const hasChoices = (c: CaptureChoices): boolean => choiceTokens(c).length > 0;
