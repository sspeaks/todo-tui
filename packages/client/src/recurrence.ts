// Recurring Items (design 3.8), as crates/todo-core/src/recurrence.rs has
// them, for FakeDaemon and the TUI: the repeat rule grammar shared by `@`
// tokens and the repeat prompt (design 2.3), and the schedule. Dates are day
// numbers, as in dates.ts.
//
// A series is the Items sharing a series_id; each instance carries the rule
// (ADR-0003). Completing an instance creates the next one with nextInstance.

import { clone, create } from "@bufbuild/protobuf";
import { DurationSchema, type Timestamp, TimestampSchema } from "@bufbuild/protobuf/wkt";
import { CreatedVia, type Item, ItemSchema, type RecurrenceRule, RecurrenceRuleSchema, Status, StatusCategory, TodoRefinement, Weekday } from "./gen/todo/v1/item_pb.js";
import { type ParseRecurrenceResponse, ParseRecurrenceResponseSchema } from "./gen/todo/v1/item_service_pb.js";
import { civil, dayNumber, dayOf, protoDate, rustTrim, weekday } from "./dates.js";
import { effectivePriority } from "./filter-eval.js";

/**
 * When a recurring Item repeats. Always concrete: the forms that depend on
 * an anchor date are a RuleSpec until resolved. The kinds are the cases of
 * the proto RecurrenceRule. Build rules with weeklyRule, monthlyRule and
 * afterCompletionRule, which keep the invariants checkRule tests.
 */
export type Rule =
  /** Every day. */
  | { kind: "daily" }
  /** On each of `days`: non-empty, sorted Monday first, without repeats. */
  | { kind: "weekly"; days: Weekday[] }
  /**
   * On `day` (1 to 31) of every month, or the month's last day when it is
   * shorter, recomputed per month: 31 gives Feb 28, then Mar 31.
   */
  | { kind: "monthly"; day: number }
  /** `days` (1 to MAX_AFTER_DAYS) after the previous instance is completed. */
  | { kind: "afterCompletion"; days: number };

/** The longest wait an after-completion rule takes, about ten years. */
export const MAX_AFTER_DAYS = 3650;

/** A rule, or the reason (for a user) it would break an invariant. */
export type Checked = { rule: Rule } | { reason: string };

const NO_DAYS = "a weekly rule needs at least one day";
const UNSORTED_DAYS = "weekly days must be sorted Monday first, without repeats";
const DAY_OF_MONTH = "the day of the month must be 1 to 31";
const AFTER_DAYS = "after completion must be 1 to 3650 days";

/** Weekly on `days`, sorted and without repeats. */
export function weeklyRule(days: Iterable<Weekday>): Checked {
  const sorted = [...new Set(days)].sort((a, b) => a - b);
  return sorted.length === 0 ? { reason: NO_DAYS } : { rule: { kind: "weekly", days: sorted } };
}

export function monthlyRule(day: number): Checked {
  return Number.isInteger(day) && day >= 1 && day <= 31 ? { rule: { kind: "monthly", day } } : { reason: DAY_OF_MONTH };
}

export function afterCompletionRule(days: number): Checked {
  return Number.isInteger(days) && days >= 1 && days <= MAX_AFTER_DAYS ? { rule: { kind: "afterCompletion", days } } : { reason: AFTER_DAYS };
}

/** Why `rule` breaks the invariants the constructors keep; undefined when it does not. */
export function checkRule(rule: Rule): string | undefined {
  switch (rule.kind) {
    case "daily":
      return undefined;
    case "weekly": {
      if (rule.days.length === 0) return NO_DAYS;
      const sorted = rule.days.every((d, i) => i === 0 || rule.days[i - 1]! < d);
      return sorted ? undefined : UNSORTED_DAYS;
    }
    case "monthly":
      return reasonOf(monthlyRule(rule.day));
    case "afterCompletion":
      return reasonOf(afterCompletionRule(rule.days));
  }
}

function reasonOf(c: Checked): string | undefined {
  return "reason" in c ? c.reason : undefined;
}

/** Scheduled on fixed dates, rather than after completion. */
export function isFixed(rule: Rule): boolean {
  return rule.kind !== "afterCompletion";
}

/**
 * The rule as an `@` token that parseRule reads back as this rule, for the
 * repeat prompt's prefill: `@daily`, `@weekdays` (Monday to Friday exactly),
 * `@mon,thu` (three-letter names, Monday first), `@15th`, and `@2w` for a
 * whole number of weeks after completion, else `@3d`. A resolved `@weekly`
 * or `@monthly` shows its day, since the rule no longer knows it came from
 * the anchor.
 */
export function ruleToken(rule: Rule): string {
  switch (rule.kind) {
    case "daily":
      return "@daily";
    case "weekly":
      if (rule.days.length === WEEKDAYS.length && rule.days.every((d, i) => d === WEEKDAYS[i])) return "@weekdays";
      return `@${rule.days.map((d) => NAMES[d - 1]!.slice(0, MIN_PREFIX)).join(",")}`;
    case "monthly":
      return `@${rule.day}${ordinalSuffix(rule.day)}`;
    case "afterCompletion":
      return rule.days % 7 === 0 ? `@${rule.days / 7}w` : `@${rule.days}d`;
  }
}

/**
 * A parsed rule before its anchor is known: `@weekly` and `@monthly` take
 * the anchor's weekday or day of the month.
 */
export type RuleSpec = { rule: Rule } | { onAnchor: "weekly" | "monthly" };

/** A repeat rule that did not parse: the expression as given, and a short reason. */
export interface RuleError {
  expr: string;
  reason: string;
}

/** Rust's Display of a RuleError. */
export function ruleErrorText(e: RuleError): string {
  return `invalid repeat rule ${debugQuoted(e.expr)}: ${e.reason}`;
}

/**
 * Rust's `{:?}` of a string (str::escape_debug): quoted, with \0, \t, \r,
 * \n, \\ and \" escaped, and every other character Rust does not print
 * (control, format, private use, unassigned, separators but the space,
 * and combining marks) as \u{hex}. Rust's tables follow its Unicode
 * version, so a code point assigned since may differ.
 */
function debugQuoted(s: string): string {
  let out = '"';
  for (const ch of s) {
    const named = DEBUG_ESCAPES[ch];
    if (named !== undefined) out += named;
    else if (ch !== " " && UNPRINTABLE.test(ch)) out += `\\u{${ch.codePointAt(0)!.toString(16)}}`;
    else out += ch;
  }
  return `${out}"`;
}

const DEBUG_ESCAPES: Record<string, string> = { "\0": "\\0", "\t": "\\t", "\r": "\\r", "\n": "\\n", "\\": "\\\\", '"': '\\"' };

const UNPRINTABLE = /[\p{Cc}\p{Cf}\p{Cs}\p{Co}\p{Cn}\p{Z}\p{Grapheme_Extend}]/u;

const NOT_A_RULE = "not a repeat rule";

/** Names must be at least this long to match a weekday by prefix, as in the date grammar. */
const MIN_PREFIX = 3;

/** Monday first, at index Weekday - 1. */
const NAMES = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];

const WEEKDAYS: readonly Weekday[] = [Weekday.MONDAY, Weekday.TUESDAY, Weekday.WEDNESDAY, Weekday.THURSDAY, Weekday.FRIDAY];

/**
 * Parses a repeat rule (design 2.3), with an optional leading `@`, as
 * `todo_core::parse_rule` does: `daily`; `weekdays` (Monday to Friday);
 * `weekly` (the anchor's weekday); weekdays separated by commas, each a
 * prefix of at least three letters (`mon,thu`, repeats collapse); `monthly`
 * (the anchor's day); a day of the month from `1st` to `31st`, with its
 * English suffix (`15th`, `22nd`, not `22th` or a bare `22`); or `N`
 * followed by `d` or `w` for `N` days or weeks after completion.
 * Case-insensitive; surrounding whitespace is ignored.
 */
export function parseRule(expr: string): { spec: RuleSpec } | RuleError {
  let s = rustTrim(expr);
  if (s.startsWith("@")) s = s.slice(1);
  // Rust's to_ascii_lowercase.
  const spec = specOf(rustTrim(s).replace(/[A-Z]/g, (c) => c.toLowerCase()));
  return typeof spec === "string" ? { expr, reason: spec } : { spec };
}

/** The spec, or why not. */
function specOf(s: string): RuleSpec | string {
  const rule = (c: Checked) => ("rule" in c ? c : c.reason);
  switch (s) {
    case "":
      return "empty repeat rule";
    case "daily":
      return { rule: { kind: "daily" } };
    case "weekdays":
      return rule(weeklyRule(WEEKDAYS));
    case "weekly":
    case "monthly":
      return { onAnchor: s };
  }
  const digits = /^[0-9]*/.exec(s)![0];
  if (digits !== "") {
    const r = numbered(digits, s.slice(digits.length));
    return typeof r === "string" ? r : { rule: r };
  }
  const names = s.split(",").map(rustTrim);
  const days = names.map(weekdayNamed);
  if (days.every((d) => d === undefined)) return NOT_A_RULE;
  const missing = days.indexOf(undefined);
  if (missing >= 0) return names[missing] === "" ? "a day is missing between commas" : `${names[missing]} is not a weekday`;
  return rule(weeklyRule(days as Weekday[]));
}

/** The weekday `name` is a long enough prefix of. */
function weekdayNamed(name: string): Weekday | undefined {
  if (name.length < MIN_PREFIX) return undefined;
  const i = NAMES.findIndex((full) => full.startsWith(name));
  return i < 0 ? undefined : ((i + 1) as Weekday);
}

/** `n` followed by `unit`: a day of the month, or days or weeks after completion. */
function numbered(n: string, unit: string): Rule | string {
  const unwrap = (c: Checked) => ("rule" in c ? c.rule : c.reason);
  // A number too large for a u32 is out of range for every unit, as it is
  // here once a double rounds it.
  const value = Number(n);
  switch (unit) {
    case "d":
      return unwrap(afterCompletionRule(value));
    case "w":
      return unwrap(afterCompletionRule(value * 7));
    case "st":
    case "nd":
    case "rd":
    case "th": {
      if (!(value >= 1 && value <= 31)) return "the day of the month must be 1st to 31st";
      const suffix = ordinalSuffix(value);
      if (unit !== suffix) return `write ${value}${suffix}`;
      return unwrap(monthlyRule(value));
    }
    case "":
      return "a number needs d or w, or a day suffix, e.g. 3d, 2w or 15th";
    default:
      return NOT_A_RULE;
  }
}

/** `st`, `nd`, `rd` or `th`, as in `1st`, `12th`, `22nd`. */
export function ordinalSuffix(n: number): string {
  if (n % 100 >= 11 && n % 100 <= 13) return "th";
  return ["th", "st", "nd", "rd"][n % 10] ?? "th";
}

/** The concrete rule of `spec` for an Item anchored on day `anchor`: its due date if any, else today. */
export function resolve(spec: RuleSpec, anchor: number): Rule {
  if ("rule" in spec) return spec.rule;
  if (spec.onAnchor === "weekly") return { kind: "weekly", days: [weekday(anchor) as Weekday] };
  return { kind: "monthly", day: civil(anchor).day };
}

/** jiff's last date, where the schedule saturates. */
const LAST_DAY = dayNumber(9999, 12, 31);

/** The day number of the last day of a month. */
function lastOfMonth(year: number, month: number): number {
  return (month === 12 ? dayNumber(year + 1, 1, 1) : dayNumber(year, month + 1, 1)) - 1;
}

/**
 * The first day on or after `date` that `rule` schedules; undefined for an
 * after-completion rule, which has no fixed dates. Near the end of the
 * calendar, where no such date exists, jiff's last date.
 */
export function firstOnOrAfter(rule: Rule, date: number): number | undefined {
  switch (rule.kind) {
    case "afterCompletion":
      return undefined;
    case "daily":
      return date;
    case "weekly":
      for (let d = date; d < date + 7 && d <= LAST_DAY; d++) if (rule.days.includes(weekday(d) as Weekday)) return d;
      return LAST_DAY;
    case "monthly": {
      const inMonth = (year: number, month: number) => dayNumber(year, month, Math.min(rule.day, civil(lastOfMonth(year, month)).day));
      const { year, month } = civil(date);
      const here = inMonth(year, month);
      if (here >= date) return here;
      const [y, m] = month === 12 ? [year + 1, 1] : [year, month + 1];
      return y > 9999 ? LAST_DAY : inMonth(y, m);
    }
  }
}

/**
 * The first occurrence a rule set on an Item due on `due` schedules, as its
 * occurrence date (design 3.8): for a fixed rule, the first scheduled date
 * on or after `due`, else today; for an after-completion rule, `due`.
 * Capture, SetRecurrence and ParseRecurrence all count it so.
 */
export function firstOccurrence(rule: Rule, due: number | undefined, today: number): number | undefined {
  return isFixed(rule) ? firstOnOrAfter(rule, due ?? today) : due;
}

/**
 * The day of the instance after the one scheduled on `slot`, completed on
 * `today`. A fixed rule skips the dates missed: the first scheduled date
 * after `slot` that is not before `today`. An after-completion rule waits
 * its days from `today`. Saturates at jiff's last date.
 */
export function nextAfter(rule: Rule, slot: number, today: number): number {
  const after = (date: number, days: number) => Math.min(date + days, LAST_DAY);
  if (rule.kind === "afterCompletion") return after(today, rule.days);
  return firstOnOrAfter(rule, Math.max(after(slot, 1), today)) ?? LAST_DAY;
}

// ---- the proto form -----------------------------------------------------------

/** The proto RecurrenceRule of a rule. */
export function ruleToProto(rule: Rule): RecurrenceRule {
  switch (rule.kind) {
    case "daily":
      return create(RecurrenceRuleSchema, { kind: { case: "daily", value: {} } });
    case "weekly":
      return create(RecurrenceRuleSchema, { kind: { case: "weekly", value: { days: [...rule.days] } } });
    case "monthly":
      return create(RecurrenceRuleSchema, { kind: { case: "monthly", value: { day: rule.day } } });
    case "afterCompletion":
      return create(RecurrenceRuleSchema, { kind: { case: "afterCompletion", value: { days: rule.days } } });
  }
}

/** A proto rule the daemon refuses: INVALID_ARGUMENT naming `path`, with description `reason`. */
export interface RuleProblem {
  path: string;
  reason: string;
}

/**
 * The rule of the proto message at `field`, as `rule_from_proto` reads it.
 * Problems name the oneof case below it (`rule.weekly.days`, and
 * `rule.kind` when no case is set). Weekly days may come in any order and
 * repeat; the rule keeps them sorted, once.
 */
export function ruleFromProto(field: string, r: RecurrenceRule): { rule: Rule } | RuleProblem {
  const at = (path: string, reason: string): RuleProblem => ({ path: `${field}.${path}`, reason });
  const checked = (path: string, c: Checked) => ("rule" in c ? c : at(path, c.reason));
  const k = r.kind;
  switch (k.case) {
    case undefined:
      return at("kind", "required field is missing");
    case "daily":
      return { rule: { kind: "daily" } };
    case "weekly":
      for (const d of k.value.days) {
        if (d === Weekday.UNSPECIFIED) return at("weekly.days", "value must not be UNSPECIFIED");
        if (Weekday[d] === undefined) return at("weekly.days", `unknown enum value ${d}`);
      }
      return checked("weekly.days", weeklyRule(k.value.days));
    case "monthly":
      return checked("monthly.day", monthlyRule(k.value.day));
    case "afterCompletion":
      return checked("after_completion.days", afterCompletionRule(k.value.days));
  }
}

/**
 * The ParseRecurrence answer for `text` (with an optional leading `@`),
 * resolved against day `anchor`, else `today`, as
 * `parse_recurrence_response` gives it: the rule and its first occurrence,
 * or the warning ParseCapture would give an `@` token with that text.
 */
export function parseRecurrenceText(text: string, anchor: number | undefined, today: number): ParseRecurrenceResponse {
  const typed = rustTrim(text);
  const parsed = parseRule(typed);
  if ("reason" in parsed) {
    // As ParseCapture words an @ token that is not a rule.
    return create(ParseRecurrenceResponseSchema, { warning: typed === "" ? parsed.reason : `${typed}: ${parsed.reason}` });
  }
  const rule = resolve(parsed.spec, anchor ?? today);
  const first = firstOccurrence(rule, anchor, today);
  return create(ParseRecurrenceResponseSchema, { rule: ruleToProto(rule), ...(first !== undefined && { first: protoDate(first) }) });
}

// ---- the next instance --------------------------------------------------------

/**
 * The next instance of the series `completed` belongs to, created at `now`
 * with id `id`, when `completed` was completed on day `today`, as
 * `next_instance` makes it; undefined when it has no rule.
 *
 * The instance copies the title, description, item type, priority, local
 * priority override, labels, parent and estimates, and nothing else (no
 * remote link or extras, tracker-only fields, waiting, private notes, focus
 * week, archive or relations). It is due on its date, which is also its
 * occurrence_date, and deferred until then unless that is today or
 * earlier. Its status is the completed Item's todo refinement, with inbox
 * becoming next. Like a capture by the user, its status and its last user
 * touch start at `now`. Its revision is 0, for the caller to set.
 */
export function nextInstance(completed: Item, id: string, today: number, now: Timestamp): Item | undefined {
  const recurrence = completed.recurrence;
  if (recurrence?.rule === undefined) return undefined;
  const read = ruleFromProto("recurrence.rule", recurrence.rule);
  // A stored rule is checked, as the store's codec checks it on read.
  if ("reason" in read) throw new Error(`${read.path}: ${read.reason}`);
  const { rule } = read;
  const slot = recurrence.occurrenceDate ?? completed.dueDate;
  const next = nextAfter(rule, slot === undefined ? today : dayOf(slot), today);
  const someday = completed.todoRefinement === TodoRefinement.SOMEDAY;
  const at = () => clone(TimestampSchema, now);
  const item = create(ItemSchema, {
    id,
    createdVia: CreatedVia.RECURRENCE,
    createdAt: at(),
    updatedAt: at(),
    title: completed.title,
    description: completed.description,
    itemType: completed.itemType,
    statusCategory: StatusCategory.TODO,
    todoRefinement: someday ? TodoRefinement.SOMEDAY : TodoRefinement.NEXT,
    status: someday ? Status.SOMEDAY : Status.NEXT,
    priority: completed.priority,
    labels: [...completed.labels],
    ...(completed.parentId !== undefined && { parentId: completed.parentId }),
    ...(completed.originalEstimate !== undefined && { originalEstimate: clone(DurationSchema, completed.originalEstimate) }),
    ...(completed.remainingEstimate !== undefined && { remainingEstimate: clone(DurationSchema, completed.remainingEstimate) }),
    dueDate: protoDate(next),
    ...(completed.localPriorityOverride !== undefined && { localPriorityOverride: completed.localPriorityOverride }),
    ...(next > today && { deferUntil: protoDate(next) }),
    recurrence: { seriesId: recurrence.seriesId, occurrenceDate: protoDate(next), rule: ruleToProto(rule) },
    staleness: { statusEnteredAt: at(), lastUserTouchAt: at() },
  });
  item.effectivePriority = effectivePriority(item);
  return item;
}
