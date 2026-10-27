// Civil dates as day numbers (days since 1970-01-01), and the labels the
// list and the detail pane show. Relative labels count from the daemon's
// today (CountsSnapshot.today), so they agree with what the views select.

import { type ProtoDate, type Timestamp, timestampDate } from "@todo-tui/client";

const DAY_MS = 86_400_000;
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

export const dayNumber = (year: number, month: number, day: number): number => Math.floor(Date.UTC(year, month - 1, day) / DAY_MS);

export const dayOf = (d: Pick<ProtoDate, "year" | "month" | "day">): number => dayNumber(d.year, d.month, d.day);

/** The local date of `at`, as a day number. */
export const localDay = (at: Date): number => dayNumber(at.getFullYear(), at.getMonth() + 1, at.getDate());

const parts = (n: number) => {
  const d = new Date(n * DAY_MS);
  return { year: d.getUTCFullYear(), month: d.getUTCMonth() + 1, day: d.getUTCDate(), weekday: d.getUTCDay() };
};

const pad = (n: number) => String(n).padStart(2, "0");

/** 2026-10-30 */
export function isoDay(n: number): string {
  const { year, month, day } = parts(n);
  return `${year}-${pad(month)}-${pad(day)}`;
}

/**
 * A short label relative to `today`: Today, Tom, the weekday within the
 * coming week, "Nov 2" within the year, else the ISO date.
 */
export function shortDay(n: number, today: number): string {
  const diff = n - today;
  if (diff === 0) return "Today";
  if (diff === 1) return "Tom";
  const p = parts(n);
  if (diff > 1 && diff < 7) return WEEKDAYS[p.weekday] ?? "";
  if (p.year === parts(today).year) return `${MONTHS[p.month - 1]} ${p.day}`;
  return isoDay(n);
}

/** Fri Oct 30 2026 */
export function longDay(n: number): string {
  const p = parts(n);
  return `${WEEKDAYS[p.weekday]} ${MONTHS[p.month - 1]} ${p.day} ${p.year}`;
}

/** "today", "in 3 days", "2 days ago". */
export function relativeDays(n: number, today: number): string {
  const diff = n - today;
  if (diff === 0) return "today";
  if (diff === 1) return "tomorrow";
  if (diff === -1) return "yesterday";
  return diff > 0 ? `in ${diff} days` : `${-diff} days ago`;
}

/** A timestamp in local time: 2026-10-26 14:03. */
export function localTime(t: Timestamp): string {
  const d = timestampDate(t);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
