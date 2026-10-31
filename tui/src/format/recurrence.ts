// How a repeat rule reads (design 3.8): in the detail pane's Repeats line,
// the repeat prompt's prefill and preview, and the quick-add summary. The
// words follow the `@` token ruleToken gives back: weeks after done only
// for a whole number of them, and Mon–Fri as "every weekday", as
// `@weekdays`.

import {
  type Item,
  type ParseRecurrenceResponse,
  type RecurrenceRule,
  Weekday,
  ordinalSuffix,
  ruleFromProto,
  ruleToken,
} from "@todo-tui/client";
import { dayLabel, dayOf } from "./dates.js";
import { singleLine } from "./text.js";

/** Monday first, at index Weekday - 1. */
const NAMES = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

const WEEKDAYS: readonly Weekday[] = [Weekday.MONDAY, Weekday.TUESDAY, Weekday.WEDNESDAY, Weekday.THURSDAY, Weekday.FRIDAY];

const ordinal = (n: number): string => `${n}${ordinalSuffix(n)}`;

const plural = (n: number, unit: string): string => `${n} ${unit}${n === 1 ? "" : "s"}`;

const isWeekdays = (days: readonly Weekday[]): boolean => days.length === WEEKDAYS.length && days.every((d, i) => d === WEEKDAYS[i]);

/**
 * The repeat prompt's prefill: the rule as ruleToken gives it, without the
 * `@`: "daily", "weekdays", "mon,thu", "15th", "2w" for whole weeks after
 * done, else "3d". A resolved weekly or monthly shows its day. "" without
 * a rule or with one the daemon would refuse.
 */
export function ruleText(rule: RecurrenceRule | undefined): string {
  if (rule === undefined) return "";
  const read = ruleFromProto("rule", rule);
  return "rule" in read ? ruleToken(read.rule).slice(1) : "";
}

/** "every day", "every weekday", "every Mon, Thu", "monthly on the 15th", "3 days after done", "2 weeks after done". */
export function describeRule(rule: RecurrenceRule): string {
  const k = rule.kind;
  switch (k.case) {
    case "daily":
      return "every day";
    case "weekly": {
      const days = k.value.days;
      return isWeekdays(days) ? "every weekday" : `every ${days.map((d) => NAMES[d - 1] ?? "?").join(", ")}`;
    }
    case "monthly":
      return `monthly on the ${ordinal(k.value.day)}`;
    case "afterCompletion": {
      const days = k.value.days;
      return `${days % 7 === 0 ? plural(days / 7, "week") : plural(days, "day")} after done`;
    }
    default:
      return "";
  }
}

/**
 * The detail pane's Repeats line: "every Mon, Thu · this one Mon Nov 2",
 * with the date this instance is scheduled for, which the next counts
 * from. Not "next": that is the instance completing this one creates (the
 * x toast's), and a missed slot is in the past. An after-completion rule
 * shows the rule alone: its slot is only the due date, and its next
 * counts from the day it is done. Undefined without a rule, as on a
 * legacy instance that never generates another.
 */
export function repeatsLine(item: Item, today: number): string | undefined {
  const recurrence = item.recurrence;
  if (recurrence?.rule === undefined) return undefined;
  const rule = describeRule(recurrence.rule);
  const slot = recurrence.rule.kind.case === "afterCompletion" ? undefined : recurrence.occurrenceDate;
  return slot ? `${rule} · this one ${dayLabel(dayOf(slot), today)}` : rule;
}

/** The repeat prompt's preview: "→ every Mon, Thu · first Mon Nov 2", or ParseRecurrence's warning. */
export function repeatPreview(res: ParseRecurrenceResponse, today: number): string {
  if (res.warning || !res.rule) return singleLine(res.warning);
  const first = res.first ? ` · first ${dayLabel(dayOf(res.first), today)}` : "";
  return `→ ${describeRule(res.rule)}${first}`;
}
