// Civil dates for FakeDaemon: day numbers (days since 1970-01-01 in the
// proleptic Gregorian calendar, as jiff counts them), today in an IANA zone,
// ISO weeks, and the capture date grammar of crates/todo-core/src/date.rs.

import type { MessageInitShape } from "@bufbuild/protobuf";
import type { Timestamp } from "@bufbuild/protobuf/wkt";
import type { DateSchema } from "./gen/google/type/date_pb.js";

export interface Ymd {
  year: number;
  month: number;
  day: number;
}

/** The day number of a date (Hinnant's days_from_civil). */
export function dayNumber(year: number, month: number, day: number): number {
  const y = month <= 2 ? year - 1 : year;
  const era = Math.floor(y / 400);
  const yoe = y - era * 400;
  const doy = Math.floor((153 * ((month + 9) % 12) + 2) / 5) + day - 1;
  const doe = yoe * 365 + Math.floor(yoe / 4) - Math.floor(yoe / 100) + doy;
  return era * 146_097 + doe - 719_468;
}

/** The date of a day number. */
export function civil(n: number): Ymd {
  const z = n + 719_468;
  const era = Math.floor(z / 146_097);
  const doe = z - era * 146_097;
  const yoe = Math.floor((doe - Math.floor(doe / 1460) + Math.floor(doe / 36_524) - Math.floor(doe / 146_096)) / 365);
  const doy = doe - (365 * yoe + Math.floor(yoe / 4) - Math.floor(yoe / 100));
  const mp = Math.floor((5 * doy + 2) / 153);
  const day = doy - Math.floor((153 * mp + 2) / 5) + 1;
  const month = mp < 10 ? mp + 3 : mp - 9;
  const year = yoe + era * 400 + (month <= 2 ? 1 : 0);
  return { year, month, day };
}

/** Monday 1 to Sunday 7; 1970-01-01 was a Thursday. */
export function weekday(n: number): number {
  return ((((n + 3) % 7) + 7) % 7) + 1;
}

function isLeap(year: number): boolean {
  return (year % 4 === 0 && year % 100 !== 0) || year % 400 === 0;
}

function daysInMonth(year: number, month: number): number {
  return month === 2 ? (isLeap(year) ? 29 : 28) : [4, 6, 9, 11].includes(month) ? 30 : 31;
}

/** Whether year, month and day name a date. */
export function isDate(year: number, month: number, day: number): boolean {
  return Number.isInteger(year) && month >= 1 && month <= 12 && day >= 1 && day <= daysInMonth(year, month);
}

/** The day number of a google.type.Date. */
export function dayOf(d: MessageInitShape<typeof DateSchema>): number {
  return dayNumber(d.year ?? 0, d.month ?? 0, d.day ?? 0);
}

/** A google.type.Date. */
export function protoDate(n: number): Ymd {
  return civil(n);
}

/** `YYYY-MM-DD`. */
export function formatDay(n: number): string {
  const { year, month, day } = civil(n);
  return `${pad(year, 4)}-${pad(month, 2)}-${pad(day, 2)}`;
}

function pad(n: number, width: number): string {
  return String(n).padStart(width, "0");
}

/** The ISO week of day `n`, `YYYY-Www` in the ISO week-year: 2027-01-01 is in 2026-W53. */
export function isoWeek(n: number): string {
  const thursday = n + 4 - weekday(n);
  const { year } = civil(thursday);
  const week = Math.floor((thursday - dayNumber(year, 1, 1)) / 7) + 1;
  return `${pad(year, 4)}-W${pad(week, 2)}`;
}

/** Whether `s` is `YYYY-Www` naming a week its year has. */
export function isIsoWeek(s: string): boolean {
  const m = /^(\d{4})-W(\d{2})$/.exec(s);
  if (!m) return false;
  const [year, week] = [Number(m[1]), Number(m[2])];
  // December 28 is always in the year's last week.
  const weeks = Number(isoWeek(dayNumber(year, 12, 28)).slice(-2));
  return week >= 1 && week <= weeks;
}

const dayFormats = new Map<string, Intl.DateTimeFormat>();

/** The day number of the civil date in `timeZone` at `epochMs`. */
export function dayIn(timeZone: string, epochMs: number): number {
  let format = dayFormats.get(timeZone);
  if (!format) {
    format = new Intl.DateTimeFormat("en-US", { timeZone, year: "numeric", month: "numeric", day: "numeric" });
    dayFormats.set(timeZone, format);
  }
  const parts = format.formatToParts(new Date(epochMs));
  const part = (type: string) => Number(parts.find((p) => p.type === type)?.value);
  return dayNumber(part("year"), part("month"), part("day"));
}

/** Milliseconds since the epoch, rounded down. */
export function epochMs(t: Timestamp): number {
  return Number(t.seconds) * 1000 + Math.floor(t.nanos / 1e6);
}

// ---- the date grammar (design 2.3) -------------------------------------------

const WEEKDAYS = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];

const MONTHS = [
  "january",
  "february",
  "march",
  "april",
  "may",
  "june",
  "july",
  "august",
  "september",
  "october",
  "november",
  "december",
];

/** The most days each month can have (February in a leap year). */
const MAX_DAYS = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/** Names must be at least this long to match a weekday or month by prefix. */
const MIN_PREFIX = 3;

const NOT_A_DATE = "not a date";
const OUT_OF_RANGE = "out of range";

/** jiff's last date. */
const LAST_DAY = dayNumber(9999, 12, 31);

/** The largest i64. */
const I64_MAX = 2n ** 63n - 1n;

/** Unicode White_Space at either end of a string. */
const RUST_WHITESPACE = /^[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+|[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+$/g;

/** Rust's `str::trim`: Unicode White_Space, which (unlike JS `trim`) has U+0085 and not U+FEFF. */
export function rustTrim(s: string): string {
  return s.replace(RUST_WHITESPACE, "");
}

/**
 * Resolves a date expression against day `today`, as `todo_core::date::parse_date`
 * does: `today`/`tod`, `tomorrow`/`tom`, a weekday (next occurrence strictly
 * after today), `+Nd`/`+Nw`/`+Nm`, `eow` (Friday), `eom`, `YYYY-MM-DD`, or a
 * month and day such as `nov2` (the first such date on or after today).
 * Case-insensitive. Returns the day number, or the reason it does not resolve.
 */
export function parseDateExpr(expr: string, today: number): { day: number } | { reason: string } {
  const result = resolve(rustTrim(expr).replace(/[A-Z]/g, (c) => c.toLowerCase()), today);
  return typeof result === "number" ? { day: result } : { reason: result };
}

/** The day, or why not. */
function resolve(s: string, today: number): number | string {
  const upTo = (n: number) => (n > LAST_DAY ? OUT_OF_RANGE : n);
  switch (s) {
    case "":
      return "empty date";
    case "today":
    case "tod":
      return today;
    case "tomorrow":
    case "tom":
      return upTo(today + 1);
    case "eow":
      return weekday(today) === 5 ? today : upTo(nextWeekday(today, 5));
    case "eom": {
      const { year, month } = civil(today);
      return dayNumber(year, month, daysInMonth(year, month));
    }
  }
  if (s.startsWith("+")) return relative(s.slice(1), today);
  const absolute = absoluteDate(s);
  if (absolute !== undefined) return absolute;
  const name = /^[a-z]*/.exec(s)![0];
  const digits = s.slice(name.length);
  if (name.length < MIN_PREFIX) return NOT_A_DATE;
  const month = MONTHS.findIndex((full) => full.startsWith(name));
  if (digits === "") {
    const day = WEEKDAYS.findIndex((full) => full.startsWith(name));
    if (day >= 0) return upTo(nextWeekday(today, day + 1));
    return month >= 0 ? "a month needs a day, e.g. nov2" : NOT_A_DATE;
  }
  if (month >= 0 && digits.length <= 2 && /^[0-9]+$/.test(digits)) return monthDay(month, Number(digits), today);
  return NOT_A_DATE;
}

/** The first day after `today` that is ISO weekday `wd`. */
function nextWeekday(today: number, wd: number): number {
  return today + ((wd - weekday(today) + 7) % 7 || 7);
}

/** `N` followed by `d`, `w` or `m`, after the `+`. */
function relative(s: string, today: number): number | string {
  const expected = "expected +N followed by d, w or m";
  const chars = [...s];
  const unit = chars.pop();
  const digits = chars.join("");
  if (unit === undefined || digits === "" || !/^[0-9]+$/.test(digits)) return expected;
  const n = BigInt(digits);
  if (n > I64_MAX) return OUT_OF_RANGE;
  // Every span past this leaves jiff's dates from any today.
  const far = n > 10_000_000n;
  switch (unit) {
    case "d":
    case "w": {
      const days = Number(n) * (unit === "w" ? 7 : 1);
      return far || today + days > LAST_DAY ? OUT_OF_RANGE : today + days;
    }
    case "m": {
      if (far) return OUT_OF_RANGE;
      const { year, month, day } = civil(today);
      const total = year * 12 + (month - 1) + Number(n);
      const [y, m] = [Math.floor(total / 12), (total % 12) + 1];
      // jiff clamps the day to the end of the month.
      return y > 9999 ? OUT_OF_RANGE : dayNumber(y, m, Math.min(day, daysInMonth(y, m)));
    }
    default:
      return expected;
  }
}

/** `YYYY-MM-DD`, or undefined when `s` does not have that shape. */
function absoluteDate(s: string): number | string | undefined {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s);
  if (!m) return undefined;
  const [year, month, day] = [Number(m[1]), Number(m[2]), Number(m[3])];
  // google.type.Date reads year 0 as "no year".
  if (year === 0) return "the year must be 1 to 9999";
  return isDate(year, month, day) ? dayNumber(year, month, day) : "no such date";
}

/**
 * The first date on or after `today` with that month (an index into MONTHS)
 * and day, looking far enough ahead to reach the next leap year for
 * February 29.
 */
function monthDay(index: number, day: number, today: number): number | string {
  if (day < 1 || day > MAX_DAYS[index]!) return `${MONTHS[index]} has no day ${day}`;
  const { year } = civil(today);
  // Leap years can be 8 years apart (2096 -> 2104).
  for (let y = year; y <= year + 8 && y <= 9999; y++) {
    if (!isDate(y, index + 1, day)) continue;
    const n = dayNumber(y, index + 1, day);
    if (n >= today) return n;
  }
  return OUT_OF_RANGE;
}
