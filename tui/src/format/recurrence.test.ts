import { create, ItemSchema, ParseRecurrenceResponseSchema, type RecurrenceRule, RecurrenceRuleSchema, Weekday } from "@todo-tui/client";
import { describe, expect, it } from "vitest";
import { dayNumber } from "./dates.js";
import { describeRule, repeatPreview, repeatsLine, ruleText } from "./recurrence.js";

const today = dayNumber(2026, 10, 26); // a Monday

const daily = (): RecurrenceRule => create(RecurrenceRuleSchema, { kind: { case: "daily", value: {} } });
const weekly = (...days: Weekday[]): RecurrenceRule => create(RecurrenceRuleSchema, { kind: { case: "weekly", value: { days } } });
const monthly = (day: number): RecurrenceRule => create(RecurrenceRuleSchema, { kind: { case: "monthly", value: { day } } });
const after = (days: number): RecurrenceRule => create(RecurrenceRuleSchema, { kind: { case: "afterCompletion", value: { days } } });

describe("repeat rules", () => {
  it("reads a rule as words, as its @ token has it", () => {
    expect(describeRule(daily())).toBe("every day");
    expect(describeRule(weekly(Weekday.MONDAY, Weekday.TUESDAY, Weekday.WEDNESDAY, Weekday.THURSDAY, Weekday.FRIDAY))).toBe("every weekday");
    expect(describeRule(weekly(Weekday.MONDAY, Weekday.THURSDAY))).toBe("every Mon, Thu");
    expect(describeRule(weekly(Weekday.SUNDAY))).toBe("every Sun");
    expect([1, 2, 3, 11, 12, 13, 15, 21, 22, 23, 31].map((d) => describeRule(monthly(d)))).toEqual([
      "monthly on the 1st",
      "monthly on the 2nd",
      "monthly on the 3rd",
      "monthly on the 11th",
      "monthly on the 12th",
      "monthly on the 13th",
      "monthly on the 15th",
      "monthly on the 21st",
      "monthly on the 22nd",
      "monthly on the 23rd",
      "monthly on the 31st",
    ]);
    // Weeks only for a whole number of them, as @2w and @3d.
    expect([1, 3, 7, 14, 10].map((d) => describeRule(after(d)))).toEqual([
      "1 day after done",
      "3 days after done",
      "1 week after done",
      "2 weeks after done",
      "10 days after done",
    ]);
    expect(describeRule(create(RecurrenceRuleSchema))).toBe("");
  });

  it("gives a rule back as the token text the repeat prompt prefills, as ruleToken without the @", () => {
    expect(ruleText(daily())).toBe("daily");
    expect(ruleText(weekly(Weekday.MONDAY, Weekday.TUESDAY, Weekday.WEDNESDAY, Weekday.THURSDAY, Weekday.FRIDAY))).toBe("weekdays");
    expect(ruleText(weekly(Weekday.MONDAY, Weekday.THURSDAY))).toBe("mon,thu");
    expect([1, 12, 22, 29].map((d) => ruleText(monthly(d)))).toEqual(["1st", "12th", "22nd", "29th"]);
    expect([3, 14].map((d) => ruleText(after(d)))).toEqual(["3d", "2w"]);
    expect(ruleText(undefined)).toBe("");
  });

  it("shows the Repeats line with the instance's own date, the rule alone without one, nothing without a rule", () => {
    const rule = weekly(Weekday.MONDAY, Weekday.THURSDAY);
    const next = create(ItemSchema, { recurrence: { seriesId: "s", rule, occurrenceDate: { year: 2026, month: 11, day: 2 } } });
    expect(repeatsLine(next, today)).toBe("every Mon, Thu · this one Mon Nov 2");
    const later = create(ItemSchema, { recurrence: { seriesId: "s", rule, occurrenceDate: { year: 2027, month: 1, day: 4 } } });
    expect(repeatsLine(later, today)).toBe("every Mon, Thu · this one Mon Jan 4 2027");
    // A missed slot is this one's, in the past: not the next.
    const missed = create(ItemSchema, { recurrence: { seriesId: "s", rule: daily(), occurrenceDate: { year: 2026, month: 10, day: 20 } } });
    expect(repeatsLine(missed, today)).toBe("every day · this one Tue Oct 20");
    expect(repeatsLine(create(ItemSchema, { recurrence: { seriesId: "s", rule: after(3) } }), today)).toBe("3 days after done");
    // An after-completion slot is only the due date; the next counts from the day it is done.
    const due = create(ItemSchema, { recurrence: { seriesId: "s", rule: after(3), occurrenceDate: { year: 2026, month: 11, day: 4 } } });
    expect(repeatsLine(due, today)).toBe("3 days after done");
    // A legacy instance (a series and no rule) never repeats.
    expect(repeatsLine(create(ItemSchema, { recurrence: { seriesId: "s" } }), today)).toBeUndefined();
    expect(repeatsLine(create(ItemSchema), today)).toBeUndefined();
  });

  it("previews the repeat prompt: the rule and its first date, or the warning", () => {
    const res = (init: Parameters<typeof create<typeof ParseRecurrenceResponseSchema>>[1]) => create(ParseRecurrenceResponseSchema, init);
    expect(repeatPreview(res({ rule: weekly(Weekday.MONDAY, Weekday.THURSDAY), first: { year: 2026, month: 11, day: 2 } }), today)).toBe(
      "→ every Mon, Thu · first Mon Nov 2",
    );
    expect(repeatPreview(res({ rule: after(14) }), today)).toBe("→ 2 weeks after done");
    expect(repeatPreview(res({ warning: "home: not a\x1b[31m repeat rule" }), today)).toBe("home: not a repeat rule");
  });
});
