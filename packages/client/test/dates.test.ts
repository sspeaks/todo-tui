import { describe, expect, it } from "vitest";
import { civil, dayIn, dayNumber, formatDay, isIsoWeek, isoWeek, parseDateExpr, weekday } from "../src/dates.js";

// The cases of crates/todo-core/src/date.rs, whose grammar FakeDaemon's
// ParseDate ports.

/** A Thursday. */
const TODAY = dayNumber(2026, 10, 22);

const day = (y: number, m: number, d: number) => formatDay(dayNumber(y, m, d));

function ok(expr: string, today = TODAY): string {
  const r = parseDateExpr(expr, today);
  if (!("day" in r)) throw new Error(`${JSON.stringify(expr)}: ${r.reason}`);
  return formatDay(r.day);
}

function reason(expr: string, today = TODAY): string | undefined {
  const r = parseDateExpr(expr, today);
  return "reason" in r ? r.reason : undefined;
}

describe("civil dates", () => {
  it("counts days as jiff does, across eras and leap years", () => {
    expect(dayNumber(1970, 1, 1)).toBe(0);
    expect(weekday(0)).toBe(4);
    for (const [y, m, d] of [
      [2026, 10, 25],
      [2000, 2, 29],
      [1, 1, 1],
      [9999, 12, 31],
      [1600, 3, 1],
      [0, 2, 29],
    ] as const) {
      expect(civil(dayNumber(y, m, d))).toEqual({ year: y, month: m, day: d });
    }
    expect(weekday(dayNumber(2026, 10, 25))).toBe(7);
    expect(weekday(dayNumber(2026, 10, 26))).toBe(1);
  });

  it("names ISO weeks in the ISO week-year", () => {
    expect(isoWeek(dayNumber(2026, 10, 25))).toBe("2026-W43");
    expect(isoWeek(dayNumber(2026, 10, 26))).toBe("2026-W44");
    expect(isoWeek(dayNumber(2027, 1, 1))).toBe("2026-W53");
    expect(isoWeek(dayNumber(2024, 12, 30))).toBe("2025-W01");
    expect(["2026-W53", "2020-W53", "2026-W01", "0000-W01"].every(isIsoWeek)).toBe(true);
    expect(["2027-W53", "2026-W00", "2026-W54", "2026-w01", "26-W01", "2026-W1"].some(isIsoWeek)).toBe(false);
  });

  it("takes today in a zone", () => {
    const at = Date.UTC(2026, 10, 2, 4, 30);
    expect(formatDay(dayIn("America/New_York", at))).toBe("2026-11-01");
    expect(formatDay(dayIn("UTC", at))).toBe("2026-11-02");
    expect(formatDay(dayIn("Asia/Tokyo", Date.UTC(2026, 9, 25, 15)))).toBe("2026-10-26");
  });
});

describe("the date grammar", () => {
  it("resolves the table", () => {
    const rows: [string, string][] = [
      ["today", day(2026, 10, 22)],
      ["tod", day(2026, 10, 22)],
      ["tomorrow", day(2026, 10, 23)],
      ["tom", day(2026, 10, 23)],
      ["mon", day(2026, 10, 26)],
      ["monday", day(2026, 10, 26)],
      ["tue", day(2026, 10, 27)],
      ["tues", day(2026, 10, 27)],
      ["wed", day(2026, 10, 28)],
      ["wednes", day(2026, 10, 28)],
      ["thu", day(2026, 10, 29)],
      ["thurs", day(2026, 10, 29)],
      ["fri", day(2026, 10, 23)],
      ["sat", day(2026, 10, 24)],
      ["sunday", day(2026, 10, 25)],
      ["+0d", day(2026, 10, 22)],
      ["+3d", day(2026, 10, 25)],
      ["+10d", day(2026, 11, 1)],
      ["+0w", day(2026, 10, 22)],
      ["+2w", day(2026, 11, 5)],
      ["+0m", day(2026, 10, 22)],
      ["+1m", day(2026, 11, 22)],
      ["+3m", day(2027, 1, 22)],
      ["+12m", day(2027, 10, 22)],
      ["+007d", day(2026, 10, 29)],
      ["eow", day(2026, 10, 23)],
      ["eom", day(2026, 10, 31)],
      ["2026-10-24", day(2026, 10, 24)],
      ["2024-02-29", day(2024, 2, 29)],
      ["1999-01-01", day(1999, 1, 1)],
      ["nov2", day(2026, 11, 2)],
      ["nov02", day(2026, 11, 2)],
      ["november2", day(2026, 11, 2)],
      ["oct22", day(2026, 10, 22)],
      ["oct21", day(2027, 10, 21)],
      ["jan1", day(2027, 1, 1)],
      ["sept30", day(2027, 9, 30)],
      ["dec31", day(2026, 12, 31)],
      ["feb29", day(2028, 2, 29)],
      ["may5", day(2027, 5, 5)],
    ];
    for (const [expr, want] of rows) expect(ok(expr), expr).toBe(want);
  });

  it("ignores case and surrounding whitespace", () => {
    for (const expr of ["TODAY", "Tom", "FRI", "  fri  ", "\tFriday\n", "Nov2", "EOW", "+3D"]) expect(reason(expr), expr).toBeUndefined();
    expect(ok("NOV02")).toBe(day(2026, 11, 2));
    expect(ok("+2W")).toBe(day(2026, 11, 5));
  });

  it("takes a weekday strictly after today, and eow as Friday", () => {
    const monday = dayNumber(2026, 10, 19);
    for (let offset = 0; offset < 7; offset++) {
      for (const [i, name] of ["mon", "tue", "wed", "thu", "fri", "sat", "sun"].entries()) {
        const r = parseDateExpr(name, monday + offset);
        expect("day" in r && weekday(r.day)).toBe(i + 1);
        const ahead = "day" in r ? r.day - (monday + offset) : 0;
        expect(ahead >= 1 && ahead <= 7).toBe(true);
      }
    }
    const eow: [number, string][] = [19, 20, 21, 22, 23, 24, 25].map((d) => [dayNumber(2026, 10, d), d <= 23 ? day(2026, 10, 23) : day(2026, 10, 30)]);
    for (const [today, want] of eow) expect(ok("eow", today)).toBe(want);
  });

  it("clamps months to their end and finds the next leap day", () => {
    expect(ok("eom", dayNumber(2028, 2, 10))).toBe(day(2028, 2, 29));
    expect(ok("+1m", dayNumber(2026, 1, 31))).toBe(day(2026, 2, 28));
    expect(ok("+1m", dayNumber(2028, 1, 31))).toBe(day(2028, 2, 29));
    expect(ok("+6m", dayNumber(2026, 8, 31))).toBe(day(2027, 2, 28));
    expect(ok("+48m", dayNumber(2028, 2, 29))).toBe(day(2032, 2, 29));
    expect(ok("feb29", dayNumber(2028, 3, 1))).toBe(day(2032, 2, 29));
    // 2100 is not a leap year.
    expect(ok("feb29", dayNumber(2097, 1, 1))).toBe(day(2104, 2, 29));
    expect(ok("jan1", dayNumber(2026, 12, 31))).toBe(day(2027, 1, 1));
  });

  it("says why an expression does not resolve", () => {
    const rows: [string, string][] = [
      ["", "empty date"],
      ["   ", "empty date"],
      ["fr", "not a date"],
      ["fridays", "not a date"],
      ["next friday", "not a date"],
      ["frié", "not a date"],
      ["2026/10/24", "not a date"],
      ["2026-1-01", "not a date"],
      ["2026-10-24x", "not a date"],
      ["2026-02-30", "no such date"],
      ["2025-02-29", "no such date"],
      ["0000-01-01", "the year must be 1 to 9999"],
      ["nov", "a month needs a day, e.g. nov2"],
      ["mon2", "not a date"],
      ["nov123", "not a date"],
      ["nov2x", "not a date"],
      ["feb30", "february has no day 30"],
      ["jan00", "january has no day 0"],
      ["+", "expected +N followed by d, w or m"],
      ["+d", "expected +N followed by d, w or m"],
      ["+3", "expected +N followed by d, w or m"],
      ["+3x", "expected +N followed by d, w or m"],
      ["+-3d", "expected +N followed by d, w or m"],
      ["+5é", "expected +N followed by d, w or m"],
      ["+99999999999999999999999d", "out of range"],
      ["+9999999999d", "out of range"],
      ["+99999999w", "out of range"],
      ["+999999m", "out of range"],
      ["+3000000d", "out of range"],
    ];
    for (const [expr, want] of rows) expect(reason(expr), expr).toBe(want);
    const last = dayNumber(9999, 12, 31);
    for (const expr of ["tom", "sat", "+1d", "+1w", "+1m", "jan1"]) expect(reason(expr, last), expr).toBe("out of range");
    expect(ok("eow", last)).toBe(day(9999, 12, 31));
    expect(ok("dec31", last)).toBe(day(9999, 12, 31));
  });
});
