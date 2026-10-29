import { describe, expect, it } from "vitest";
import { firstDay, lastDay, monthOf, monthTitle, monthWeeks, nextMonth } from "./calendar.js";
import { civil, dayNumber } from "./dates.js";

const days = (weeks: (number | undefined)[][]) => weeks.map((w) => w.map((n) => (n === undefined ? 0 : civil(n).day)));

describe("calendar", () => {
  it("lays a month out Sunday first, in six weeks", () => {
    // 2026-10-01 is a Thursday.
    const oct = days(monthWeeks({ year: 2026, month: 10 }));
    expect(oct).toHaveLength(6);
    expect(oct[0]).toEqual([0, 0, 0, 0, 1, 2, 3]);
    expect(oct[4]).toEqual([25, 26, 27, 28, 29, 30, 31]);
    expect(oct[5]).toEqual([0, 0, 0, 0, 0, 0, 0]);
    // 2026-11-01 is a Sunday: it opens the first week.
    const nov = days(monthWeeks({ year: 2026, month: 11 }));
    expect(nov[0]).toEqual([1, 2, 3, 4, 5, 6, 7]);
    expect(nov[4]).toEqual([29, 30, 0, 0, 0, 0, 0]);
    // 2026-08-01 is a Saturday: the month needs all six weeks.
    const aug = days(monthWeeks({ year: 2026, month: 8 }));
    expect(aug[0]).toEqual([0, 0, 0, 0, 0, 0, 1]);
    expect(aug[5]).toEqual([30, 31, 0, 0, 0, 0, 0]);
    // February 2026 starts on a Sunday and fits four weeks; the rest are blank.
    const feb = days(monthWeeks({ year: 2026, month: 2 }));
    expect(feb[0]).toEqual([1, 2, 3, 4, 5, 6, 7]);
    expect(feb[3]).toEqual([22, 23, 24, 25, 26, 27, 28]);
    expect(feb.slice(4).flat().every((d) => d === 0)).toBe(true);
  });

  it("steps across the year end and knows each month's bounds", () => {
    expect(nextMonth({ year: 2026, month: 12 })).toEqual({ year: 2027, month: 1 });
    expect(nextMonth({ year: 2026, month: 10 })).toEqual({ year: 2026, month: 11 });
    expect(monthOf(dayNumber(2026, 10, 29))).toEqual({ year: 2026, month: 10 });
    expect(firstDay({ year: 2026, month: 10 })).toBe(dayNumber(2026, 10, 1));
    expect(lastDay({ year: 2026, month: 12 })).toBe(dayNumber(2026, 12, 31));
    expect(lastDay({ year: 2028, month: 2 })).toBe(dayNumber(2028, 2, 29));
    expect(monthTitle({ year: 2026, month: 10 })).toBe("October 2026");
  });
});
