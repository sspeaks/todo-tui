import { create } from "@bufbuild/protobuf";
import { DurationSchema, TimestampSchema } from "@bufbuild/protobuf/wkt";
import { describe, expect, it } from "vitest";
import { dayNumber, protoDate } from "../src/dates.js";
import { DateSchema } from "../src/gen/google/type/date_pb.js";
import {
  ArchiveReason,
  CreatedVia,
  type Item,
  ItemSchema,
  LinkState,
  Priority,
  type RecurrenceRule,
  RecurrenceRuleSchema,
  RecurrenceRule_WeeklySchema,
  ResolutionKind,
  Status,
  StatusCategory,
  TodoRefinement,
  Weekday,
} from "../src/gen/todo/v1/item_pb.js";
import { ParseRecurrenceResponseSchema } from "../src/gen/todo/v1/item_service_pb.js";
import { RelationKind } from "../src/gen/todo/v1/relation_pb.js";
import {
  MAX_AFTER_DAYS,
  type Rule,
  type RuleSpec,
  afterCompletionRule,
  checkRule,
  firstOnOrAfter,
  monthlyRule,
  nextAfter,
  nextInstance,
  ordinalSuffix,
  parseRecurrenceText,
  parseRule,
  resolve,
  ruleErrorText,
  ruleFromProto,
  ruleToProto,
  ruleToken,
  weeklyRule,
} from "../src/recurrence.js";

// The cases of crates/todo-core/src/recurrence.rs, whose rule engine
// FakeDaemon ports, and the rule cases of crates/todo-proto/src/convert.rs.

const { MONDAY, TUESDAY, WEDNESDAY, THURSDAY, FRIDAY, SATURDAY, SUNDAY } = Weekday;
const ALL = [MONDAY, TUESDAY, WEDNESDAY, THURSDAY, FRIDAY, SATURDAY, SUNDAY];

const date = dayNumber;

/** A Thursday. */
const TODAY = date(2026, 10, 22);

/** jiff's Date::MAX. */
const MAX = date(9999, 12, 31);

function ok(expr: string): RuleSpec {
  const r = parseRule(expr);
  if (!("spec" in r)) throw new Error(`${JSON.stringify(expr)}: ${r.reason}`);
  return r.spec;
}

function rule(expr: string): Rule {
  const spec = ok(expr);
  if (!("rule" in spec)) throw new Error(`${JSON.stringify(expr)}: ${JSON.stringify(spec)}`);
  return spec.rule;
}

const daily: Rule = { kind: "daily" };
const weekly = (days: Weekday[]): Rule => ({ kind: "weekly", days });
const monthly = (day: number): Rule => ({ kind: "monthly", day });
const after = (days: number): Rule => ({ kind: "afterCompletion", days });

function reason(expr: string): string | undefined {
  const r = parseRule(expr);
  return "reason" in r ? r.reason : undefined;
}

describe("parseRule", () => {
  it("reads the grammar table", () => {
    const rows: [string, Rule][] = [
      ["daily", daily],
      ["weekdays", weekly([MONDAY, TUESDAY, WEDNESDAY, THURSDAY, FRIDAY])],
      ["mon", weekly([MONDAY])],
      ["monday", weekly([MONDAY])],
      ["mon,thu", weekly([MONDAY, THURSDAY])],
      ["thu,mon", weekly([MONDAY, THURSDAY])],
      ["tues,thurs", weekly([TUESDAY, THURSDAY])],
      ["sat,sun", weekly([SATURDAY, SUNDAY])],
      ["sun,sat,fri", weekly([FRIDAY, SATURDAY, SUNDAY])],
      ["wed,wednesday,wedn", weekly([WEDNESDAY])],
      ["mon, thu", weekly([MONDAY, THURSDAY])],
      ["1st", monthly(1)],
      ["2nd", monthly(2)],
      ["3rd", monthly(3)],
      ["4th", monthly(4)],
      ["11th", monthly(11)],
      ["12th", monthly(12)],
      ["13th", monthly(13)],
      ["15th", monthly(15)],
      ["21st", monthly(21)],
      ["22nd", monthly(22)],
      ["23rd", monthly(23)],
      ["31st", monthly(31)],
      ["01st", monthly(1)],
      ["1d", after(1)],
      ["3d", after(3)],
      ["2w", after(14)],
      ["3650d", after(3650)],
      ["521w", after(3647)],
      ["007d", after(7)],
    ];
    for (const [expr, want] of rows) {
      expect(rule(expr), expr).toEqual(want);
      expect(rule(`@${expr}`), `@${expr}`).toEqual(want);
      expect(checkRule(want), expr).toBeUndefined();
    }
    expect(ok("weekly")).toEqual({ onAnchor: "weekly" });
    expect(ok("@monthly")).toEqual({ onAnchor: "monthly" });
  });

  it("ignores case and whitespace", () => {
    expect(rule("DAILY")).toEqual(daily);
    expect(rule("@Daily")).toEqual(daily);
    expect(rule("  @weekDays \t")).toEqual(rule("weekdays"));
    expect(rule("MON,Thu")).toEqual(weekly([MONDAY, THURSDAY]));
    expect(rule("15TH")).toEqual(monthly(15));
    expect(rule("3D")).toEqual(after(3));
    expect(rule("2W")).toEqual(after(14));
    expect(ok("WEEKLY")).toEqual({ onAnchor: "weekly" });
    // Whitespace after the `@` too, as `^ fri` is a date.
    expect(rule("@ daily")).toEqual(daily);
    expect(rule(" @ 15th ")).toEqual(monthly(15));
    expect(rule("@\t3d")).toEqual(after(3));
    expect(rule("@ mon")).toEqual(weekly([MONDAY]));
    expect(ok("@ weekly")).toEqual({ onAnchor: "weekly" });
  });

  it("gives the reason a rule does not parse", () => {
    const NOT_A_RULE = "not a repeat rule";
    const rows: [string, string][] = [
      ["", "empty repeat rule"],
      ["@", "empty repeat rule"],
      ["  ", "empty repeat rule"],
      ["home", NOT_A_RULE],
      ["@home", NOT_A_RULE],
      ["weekday", NOT_A_RULE],
      ["weekends", NOT_A_RULE],
      ["yearly", NOT_A_RULE],
      ["mo", NOT_A_RULE],
      ["mondays", NOT_A_RULE],
      ["@@daily", NOT_A_RULE],
      ["daily,mon", "daily is not a weekday"],
      ["mon,fr", "fr is not a weekday"],
      ["mon,home", "home is not a weekday"],
      ["mon,", "a day is missing between commas"],
      ["mon,,thu", "a day is missing between commas"],
      [",mon", "a day is missing between commas"],
      ["0th", "the day of the month must be 1st to 31st"],
      ["32nd", "the day of the month must be 1st to 31st"],
      ["99999999999st", "the day of the month must be 1st to 31st"],
      ["1th", "write 1st"],
      ["2th", "write 2nd"],
      ["3th", "write 3rd"],
      ["4st", "write 4th"],
      ["11st", "write 11th"],
      ["12nd", "write 12th"],
      ["13rd", "write 13th"],
      ["21th", "write 21st"],
      ["22th", "write 22nd"],
      ["15", "a number needs d or w, or a day suffix, e.g. 3d, 2w or 15th"],
      ["0d", "after completion must be 1 to 3650 days"],
      ["0w", "after completion must be 1 to 3650 days"],
      ["3651d", "after completion must be 1 to 3650 days"],
      ["522w", "after completion must be 1 to 3650 days"],
      ["99999999999d", "after completion must be 1 to 3650 days"],
      ["999999999w", "after completion must be 1 to 3650 days"],
      ["3m", NOT_A_RULE],
      ["3dd", NOT_A_RULE],
      ["+3d", NOT_A_RULE],
      ["15th,mon", NOT_A_RULE],
      ["frié", NOT_A_RULE],
    ];
    for (const [expr, want] of rows) {
      const err = parseRule(expr);
      if ("spec" in err) throw new Error(`${JSON.stringify(expr)} parsed`);
      expect(err.expr).toBe(expr);
      expect(err.reason, expr).toBe(want);
    }
    const err = parseRule("@home");
    expect("reason" in err && ruleErrorText(err)).toBe('invalid repeat rule "@home": not a repeat rule');
    // The expression is quoted as Rust's {:?} quotes it, not as JSON.
    const quoted: [string, string][] = [
      ["@\u0002x", '"@\\u{2}x"'],
      ["﻿daily", '"\\u{feff}daily"'],
      ["a\u007f\u0085­​", '"a\\u{7f}\\u{85}\\u{ad}\\u{200b}"'],
      ["\b\f\u001f", '"\\u{8}\\u{c}\\u{1f}"'],
      ["\0\t\r\n\\\"'", "\"\\0\\t\\r\\n\\\\\\\"'\""],
      ["é  　 ", '"e\\u{301} \\u{a0}\\u{3000}\\u{2028}"'],
      ["͸\ud800", '"\\u{e000}\\u{378}\\u{d800}"'],
      ["frié 😀", '"frié 😀"'],
    ];
    for (const [expr, want] of quoted) expect(ruleErrorText({ expr, reason: NOT_A_RULE }), expr).toBe(`invalid repeat rule ${want}: ${NOT_A_RULE}`);
    expect(reason("15TH,MON")).toBe(NOT_A_RULE);
  });
});

describe("rules", () => {
  it("gives ordinal suffixes", () => {
    const rows: [number, string][] = [
      [1, "st"],
      [2, "nd"],
      [3, "rd"],
      [4, "th"],
      [10, "th"],
      [11, "th"],
      [12, "th"],
      [13, "th"],
      [14, "th"],
      [20, "th"],
      [21, "st"],
      [22, "nd"],
      [23, "rd"],
      [24, "th"],
      [30, "th"],
      [31, "st"],
      [101, "st"],
      [111, "th"],
    ];
    for (const [n, want] of rows) expect(ordinalSuffix(n), String(n)).toBe(want);
  });

  it("keeps the invariants in the constructors and checkRule", () => {
    const NO_DAYS = { reason: "a weekly rule needs at least one day" };
    const DAY_OF_MONTH = { reason: "the day of the month must be 1 to 31" };
    const AFTER_DAYS = { reason: "after completion must be 1 to 3650 days" };
    const UNSORTED = "weekly days must be sorted Monday first, without repeats";
    expect(weeklyRule([THURSDAY, MONDAY, THURSDAY])).toEqual({ rule: weekly([MONDAY, THURSDAY]) });
    expect(weeklyRule([])).toEqual(NO_DAYS);
    expect(monthlyRule(0)).toEqual(DAY_OF_MONTH);
    expect(monthlyRule(31)).toEqual({ rule: monthly(31) });
    expect(monthlyRule(32)).toEqual(DAY_OF_MONTH);
    expect(monthlyRule(256 + 15)).toEqual(DAY_OF_MONTH);
    expect(afterCompletionRule(0)).toEqual(AFTER_DAYS);
    expect(afterCompletionRule(3650)).toEqual({ rule: after(3650) });
    expect(afterCompletionRule(3651)).toEqual(AFTER_DAYS);
    expect(afterCompletionRule(65536 + 3)).toEqual(AFTER_DAYS);

    expect(checkRule(weekly([]))).toBe(NO_DAYS.reason);
    expect(checkRule(weekly([THURSDAY, MONDAY]))).toBe(UNSORTED);
    expect(checkRule(weekly([MONDAY, MONDAY]))).toBe(UNSORTED);
    expect(checkRule(monthly(0))).toBe(DAY_OF_MONTH.reason);
    expect(checkRule(monthly(32))).toBe(DAY_OF_MONTH.reason);
    expect(checkRule(after(0))).toBe(AFTER_DAYS.reason);
    expect(checkRule(after(3651))).toBe(AFTER_DAYS.reason);
    expect(checkRule(daily)).toBeUndefined();
  });

  // In place of the_serde_form_is_fixed: the TS rule's persisted form is the proto message.
  it("round-trips through the proto form", () => {
    const rules = [daily, weekly(ALL), monthly(31), after(3650)];
    for (const r of rules) expect(ruleFromProto("rule", ruleToProto(r))).toEqual({ rule: r });
    expect(ruleToProto(weekly([MONDAY, SUNDAY]))).toEqual(create(RecurrenceRuleSchema, { kind: { case: "weekly", value: { days: [MONDAY, SUNDAY] } } }));
    // Every weekday is its ISO number.
    expect(ALL).toEqual([1, 2, 3, 4, 5, 6, 7]);
    // Days are sorted and collapse on the way in.
    const proto = create(RecurrenceRuleSchema, { kind: { case: "weekly", value: { days: [THURSDAY, MONDAY, THURSDAY] } } });
    expect(ruleFromProto("rule", proto)).toEqual({ rule: weekly([MONDAY, THURSDAY]) });
  });

  it("names the oneof case in a proto rule's errors", () => {
    const fails = (kind: RecurrenceRule["kind"]) => ruleFromProto("rule", create(RecurrenceRuleSchema, { kind }));
    const weeklyKind = (days: Weekday[]): RecurrenceRule["kind"] => ({ case: "weekly", value: create(RecurrenceRule_WeeklySchema, { days }) });
    expect(fails({ case: undefined })).toEqual({ path: "rule.kind", reason: "required field is missing" });
    expect(fails(weeklyKind([]))).toEqual({ path: "rule.weekly.days", reason: "a weekly rule needs at least one day" });
    expect(fails(weeklyKind([1, 0]))).toEqual({ path: "rule.weekly.days", reason: "value must not be UNSPECIFIED" });
    expect(fails(weeklyKind([9 as Weekday]))).toEqual({ path: "rule.weekly.days", reason: "unknown enum value 9" });
    const invalid: [RecurrenceRule, string][] = [
      [ruleToProto(monthly(0)), "rule.monthly.day"],
      [ruleToProto(monthly(32)), "rule.monthly.day"],
      [ruleToProto(monthly(255 + 2)), "rule.monthly.day"],
      [ruleToProto(after(0)), "rule.after_completion.days"],
      [ruleToProto(after(3651)), "rule.after_completion.days"],
      [ruleToProto(after(65535 + 2)), "rule.after_completion.days"],
    ];
    for (const [proto, path] of invalid) {
      const r = ruleFromProto("rule", proto);
      expect("path" in r && r.path).toBe(path);
    }
  });

  it("gives a token that reads back as the rule", () => {
    const rows: [Rule, string][] = [
      [daily, "@daily"],
      [weekly([MONDAY, TUESDAY, WEDNESDAY, THURSDAY, FRIDAY]), "@weekdays"],
      [weekly([MONDAY, THURSDAY]), "@mon,thu"],
      [weekly([THURSDAY]), "@thu"],
      [weekly([FRIDAY, SATURDAY, SUNDAY]), "@fri,sat,sun"],
      [weekly([MONDAY, TUESDAY, WEDNESDAY, THURSDAY, FRIDAY, SATURDAY]), "@mon,tue,wed,thu,fri,sat"],
      [weekly(ALL), "@mon,tue,wed,thu,fri,sat,sun"],
      [monthly(1), "@1st"],
      [monthly(2), "@2nd"],
      [monthly(3), "@3rd"],
      [monthly(11), "@11th"],
      [monthly(15), "@15th"],
      [monthly(22), "@22nd"],
      [monthly(31), "@31st"],
      [after(1), "@1d"],
      [after(3), "@3d"],
      [after(7), "@1w"],
      [after(14), "@2w"],
      [after(15), "@15d"],
      [after(3647), "@521w"],
      [after(3650), "@3650d"],
    ];
    for (const [want, token] of rows) {
      expect(ruleToken(want), JSON.stringify(want)).toBe(token);
      expect(rule(token), token).toEqual(want);
    }
    // Every rule of every kind reads back.
    const every: Rule[] = [daily];
    for (let day = 1; day <= 31; day++) every.push(monthly(day));
    for (let days = 1; days <= MAX_AFTER_DAYS; days++) every.push(after(days));
    for (let mask = 1; mask < 128; mask++) every.push(weekly(ALL.filter((_, i) => (mask & (1 << i)) !== 0)));
    for (const want of every) expect(rule(ruleToken(want)), ruleToken(want)).toEqual(want);
    // The anchor forms show what they resolved to.
    const friday = date(2026, 10, 23);
    expect(ruleToken(resolve(ok("@weekly"), friday))).toBe("@fri");
    expect(ruleToken(resolve(ok("@monthly"), friday))).toBe("@23rd");
  });

  it("resolves against the anchor", () => {
    const friday = date(2026, 10, 23);
    expect(resolve({ onAnchor: "weekly" }, friday)).toEqual(weekly([FRIDAY]));
    expect(resolve({ onAnchor: "monthly" }, date(2026, 1, 31))).toEqual(monthly(31));
    expect(resolve({ onAnchor: "monthly" }, date(2026, 2, 1))).toEqual(monthly(1));
    expect(resolve({ rule: daily }, friday)).toEqual(daily);
    for (let d = 0; d < 7; d++) {
      const anchor = TODAY + d;
      expect(firstOnOrAfter(resolve({ onAnchor: "weekly" }, anchor), anchor)).toBe(anchor);
    }
  });
});

describe("the schedule", () => {
  it("finds the first date on or after a day for each rule", () => {
    const rows: [Rule, number, number | undefined][] = [
      [daily, TODAY, TODAY],
      [weekly([THURSDAY]), TODAY, TODAY],
      [weekly([FRIDAY]), TODAY, date(2026, 10, 23)],
      // The week wraps.
      [weekly([MONDAY]), TODAY, date(2026, 10, 26)],
      [weekly([MONDAY, WEDNESDAY]), TODAY, date(2026, 10, 26)],
      [weekly([WEDNESDAY]), TODAY, date(2026, 10, 28)],
      [weekly([SUNDAY]), date(2026, 12, 28), date(2027, 1, 3)],
      [monthly(22), TODAY, TODAY],
      [monthly(25), TODAY, date(2026, 10, 25)],
      [monthly(1), TODAY, date(2026, 11, 1)],
      [monthly(31), TODAY, date(2026, 10, 31)],
      [monthly(31), date(2026, 11, 5), date(2026, 11, 30)],
      [monthly(15), date(2026, 12, 20), date(2027, 1, 15)],
      [monthly(30), date(2026, 2, 1), date(2026, 2, 28)],
      [monthly(29), date(2028, 2, 1), date(2028, 2, 29)],
      [after(3), TODAY, undefined],
    ];
    for (const [r, from, want] of rows) expect(firstOnOrAfter(r, from), `${JSON.stringify(r)} from ${from}`).toBe(want);
  });

  /** Successive instances of `rule` from `slot`, each completed on its day. */
  function series(r: Rule, slot: number, n: number): number[] {
    const out: number[] = [];
    for (let s = slot; out.length < n; ) out.push((s = nextAfter(r, s, s)));
    return out;
  }

  it("clamps months to their last day, then recovers", () => {
    const day31 = monthly(31);
    expect(series(day31, date(2026, 12, 31), 5)).toEqual([date(2027, 1, 31), date(2027, 2, 28), date(2027, 3, 31), date(2027, 4, 30), date(2027, 5, 31)]);
    // A leap year.
    expect(series(day31, date(2028, 1, 31), 2)).toEqual([date(2028, 2, 29), date(2028, 3, 31)]);
    const day29 = monthly(29);
    expect(series(day29, date(2027, 1, 29), 2)).toEqual([date(2027, 2, 28), date(2027, 3, 29)]);
    expect(series(day29, date(2028, 1, 29), 2)).toEqual([date(2028, 2, 29), date(2028, 3, 29)]);
    // 2100 is not a leap year.
    expect(series(monthly(30), date(2100, 1, 30), 2)).toEqual([date(2100, 2, 28), date(2100, 3, 30)]);
    // Year end.
    expect(series(monthly(15), date(2026, 12, 15), 1)).toEqual([date(2027, 1, 15)]);
  });

  it("wraps weeks", () => {
    const monThu = weekly([MONDAY, THURSDAY]);
    expect(series(monThu, date(2026, 10, 22), 4)).toEqual([date(2026, 10, 26), date(2026, 10, 29), date(2026, 11, 2), date(2026, 11, 5)]);
    expect(series(weekly([SUNDAY]), date(2026, 12, 27), 2)).toEqual([date(2027, 1, 3), date(2027, 1, 10)]);
    expect(series(daily, date(2026, 12, 30), 3)).toEqual([date(2026, 12, 31), date(2027, 1, 1), date(2027, 1, 2)]);
  });

  it("skips the weekend for weekdays", () => {
    const weekdays = rule("weekdays");
    const friday = date(2026, 10, 23);
    expect(series(weekdays, friday, 2)).toEqual([date(2026, 10, 26), date(2026, 10, 27)]);
    // Completed on Saturday or Sunday: the next is Monday.
    for (const weekend of [date(2026, 10, 24), date(2026, 10, 25)]) expect(nextAfter(weekdays, friday, weekend)).toBe(date(2026, 10, 26));
    expect(firstOnOrAfter(weekdays, date(2026, 10, 24))).toBe(date(2026, 10, 26));
  });

  it("skips missed dates", () => {
    // Due Monday Oct 5, completed weeks late on Thursday Oct 22.
    const slot = date(2026, 10, 5);
    expect(nextAfter(weekly([MONDAY]), slot, TODAY)).toBe(date(2026, 10, 26));
    expect(nextAfter(weekly([MONDAY, THURSDAY]), slot, TODAY), "today itself is scheduled").toBe(TODAY);
    expect(nextAfter(daily, slot, TODAY)).toBe(TODAY);
    expect(nextAfter(monthly(5), slot, TODAY)).toBe(date(2026, 11, 5));
    expect(nextAfter(monthly(31), date(2026, 1, 31), TODAY)).toBe(date(2026, 10, 31));
    // Completed early: still after the slot, not before today.
    const early = date(2026, 10, 20);
    expect(nextAfter(daily, TODAY, early)).toBe(date(2026, 10, 23));
    expect(nextAfter(weekly([THURSDAY]), TODAY, early)).toBe(date(2026, 10, 29));
    // After completion counts from today, whatever the slot.
    expect(nextAfter(after(3), slot, TODAY)).toBe(date(2026, 10, 25));
    expect(nextAfter(after(3), date(2027, 1, 1), TODAY)).toBe(date(2026, 10, 25));
  });

  it("saturates at the end of the calendar", () => {
    expect(nextAfter(daily, MAX, TODAY)).toBe(MAX);
    expect(nextAfter(after(10), TODAY, MAX)).toBe(MAX);
    expect(firstOnOrAfter(monthly(1), date(9999, 12, 2))).toBe(MAX);
    expect(firstOnOrAfter(weekly([MONDAY]), date(9999, 12, 28))).toBe(MAX);
  });
});

describe("parseRecurrenceText", () => {
  it("answers as ParseRecurrence does", () => {
    const friday = date(2026, 10, 23);
    const answer = (text: string, anchor?: number) => parseRecurrenceText(text, anchor, TODAY);
    const ok = (r: Rule, first: number | undefined) =>
      create(ParseRecurrenceResponseSchema, { rule: ruleToProto(r), ...(first !== undefined && { first: protoDate(first) }) });
    expect(answer("@daily")).toEqual(ok(daily, TODAY));
    expect(answer(" daily ")).toEqual(ok(daily, TODAY));
    expect(answer("weekly", friday)).toEqual(ok(weekly([FRIDAY]), friday));
    expect(answer("@mon,thu", friday)).toEqual(ok(weekly([MONDAY, THURSDAY]), date(2026, 10, 26)));
    expect(answer("monthly")).toEqual(ok(monthly(22), TODAY));
    expect(answer("3d")).toEqual(ok(after(3), undefined));
    expect(answer("@2w", friday)).toEqual(ok(after(14), friday));
    const warning = (text: string) => answer(text).warning;
    expect(warning("@home")).toBe("@home: not a repeat rule");
    expect(warning(" home ")).toBe("home: not a repeat rule");
    expect(warning("")).toBe("empty repeat rule");
    expect(warning("@")).toBe("@: empty repeat rule");
    const failed = answer("@22th");
    expect([failed.rule, failed.first]).toEqual([undefined, undefined]);
    expect(failed.warning).toBe("@22th: write 22nd");
  });
});

describe("nextInstance", () => {
  const id = (n: number) => String(n).padStart(26, "0");
  const ts = (seconds: number) => create(TimestampSchema, { seconds: BigInt(seconds) });
  const mins = (m: number) => create(DurationSchema, { seconds: BigInt(m * 60) });
  const pdate = (n: number) => create(DateSchema, protoDate(n));

  /** A completed instance with every field set. */
  function completed(r: Rule, occurrenceDate: number | undefined): Item {
    return create(ItemSchema, {
      id: id(1),
      createdVia: CreatedVia.TUI,
      createdAt: ts(1_000),
      updatedAt: ts(2_000),
      revision: 9n,
      remoteLink: {
        trackerKind: "jira",
        instanceId: "work-jira",
        remoteId: "10001",
        key: "PLAT-530",
        url: "https://jira.example.com/browse/PLAT-530",
        linkState: LinkState.OK,
      },
      title: "water plants",
      description: "the ferns too",
      itemType: "chore",
      remoteStatusName: "Done",
      priority: Priority.HIGH,
      assignee: { ref: { case: "personId", value: id(2) } },
      reporter: { ref: { case: "personId", value: id(3) } },
      labels: ["home", "garden"],
      components: ["api"],
      fixVersions: ["1.2"],
      dueDate: protoDate(date(2026, 10, 19)),
      parentId: id(4),
      sprint: "Sprint 7",
      storyPoints: 1,
      originalEstimate: mins(30),
      remainingEstimate: mins(10),
      resolution: "Fixed",
      remoteCreatedAt: ts(500),
      remoteUpdatedAt: ts(600),
      statusCategory: StatusCategory.DONE,
      resolutionKind: ResolutionKind.DONE,
      todoRefinement: TodoRefinement.NEXT,
      status: Status.DONE,
      waiting: { on: { ref: { case: "freeText", value: "dana" } }, reason: "hose", since: protoDate(date(2026, 10, 1)) },
      deferUntil: protoDate(date(2026, 10, 18)),
      privateNotes: "notes",
      focusWeek: "2026-W43",
      localPriorityOverride: Priority.LOW,
      archived: { at: ts(1_500), reason: ArchiveReason.USER },
      recurrence: {
        seriesId: id(7),
        ...(occurrenceDate !== undefined && { occurrenceDate: protoDate(occurrenceDate) }),
        rule: ruleToProto(r),
      },
      remoteExtras: { customfield_1: { kind: { case: "numberValue", value: 1 } } },
      isMine: true,
      scopes: ["mine"],
      subtasksTotal: 2,
      subtasksDone: 1,
      parentTitle: "garden",
      staleness: { lastUserTouchAt: ts(2_000) },
      relations: [{ fromId: id(1), toId: id(5), kind: RelationKind.BLOCKS }],
    });
  }

  const next = (done: Item) => {
    const item = nextInstance(done, id(10), TODAY, ts(9_000));
    if (!item) throw new Error("no next instance");
    return item;
  };

  it("copies what the plan lists", () => {
    const now = ts(9_000);
    const done = completed(weekly([MONDAY, THURSDAY]), date(2026, 10, 19));
    const item = next(done);
    const want = create(ItemSchema, {
      id: id(10),
      createdVia: CreatedVia.RECURRENCE,
      createdAt: now,
      updatedAt: now,
      title: "water plants",
      description: "the ferns too",
      itemType: "chore",
      priority: Priority.HIGH,
      labels: ["home", "garden"],
      parentId: id(4),
      originalEstimate: mins(30),
      remainingEstimate: mins(10),
      // Due Monday Oct 19, completed Thursday Oct 22, which is scheduled.
      dueDate: protoDate(TODAY),
      statusCategory: StatusCategory.TODO,
      todoRefinement: TodoRefinement.NEXT,
      status: Status.NEXT,
      localPriorityOverride: Priority.LOW,
      effectivePriority: Priority.LOW,
      recurrence: { seriesId: id(7), occurrenceDate: protoDate(TODAY), rule: ruleToProto(weekly([MONDAY, THURSDAY])) },
      staleness: { statusEnteredAt: now, lastUserTouchAt: now },
    });
    expect(item).toEqual(want);
    // Not deferred: it is due today.
    expect(item.deferUntil).toBeUndefined();
    expect(item.revision).toBe(0n);
    expect(item.relations).toEqual([]);
    expect(item.remoteLink).toBeUndefined();
    // Nothing is shared with the completed Item.
    item.labels.push("x");
    item.staleness!.statusEnteredAt!.seconds = 1n;
    expect(done.labels).toEqual(["home", "garden"]);
    expect(item.createdAt!.seconds).toBe(9_000n);
  });

  it("is deferred until a later date", () => {
    const item = next(completed(weekly([MONDAY]), date(2026, 10, 19)));
    expect(item.dueDate).toEqual(pdate(date(2026, 10, 26)));
    expect(item.deferUntil).toEqual(pdate(date(2026, 10, 26)));
    expect(item.recurrence?.occurrenceDate).toEqual(pdate(date(2026, 10, 26)));
  });

  it("takes the slot from the occurrence, then the due date, then today", () => {
    // occurrence_date wins over due_date.
    let item = next(completed(daily, date(2026, 10, 25)));
    expect(item.dueDate).toEqual(pdate(date(2026, 10, 26)));
    // Without one, the due date (Oct 19, missed: today).
    item = next(completed(daily, undefined));
    expect(item.dueDate).toEqual(pdate(TODAY));
    // Without either, today: tomorrow.
    const done = completed(daily, undefined);
    delete done.dueDate;
    item = next(done);
    expect(item.dueDate).toEqual(pdate(date(2026, 10, 23)));
    expect(item.deferUntil).toEqual(pdate(date(2026, 10, 23)));
  });

  it("waits after completion without a due date", () => {
    const done = completed(after(3), undefined);
    delete done.dueDate;
    const item = next(done);
    expect(item.dueDate).toEqual(pdate(date(2026, 10, 25)));
    expect(item.deferUntil).toEqual(pdate(date(2026, 10, 25)));
    expect(item.recurrence?.occurrenceDate).toEqual(pdate(date(2026, 10, 25)));
    expect(item.recurrence?.rule).toEqual(ruleToProto(after(3)));
    expect(item.recurrence?.seriesId).toBe(id(7));
  });

  it("takes its status from the refinement", () => {
    const rows: [TodoRefinement, Status][] = [
      [TodoRefinement.INBOX, Status.NEXT],
      [TodoRefinement.NEXT, Status.NEXT],
      [TodoRefinement.SOMEDAY, Status.SOMEDAY],
    ];
    for (const [refinement, want] of rows) {
      const done = completed(daily, undefined);
      done.todoRefinement = refinement;
      for (const [closed, kind] of [
        [Status.DONE, ResolutionKind.DONE],
        [Status.CANCELLED, ResolutionKind.CANCELLED],
      ] as const) {
        done.status = closed;
        done.resolutionKind = kind;
        const item = next(done);
        expect(item.status, `${refinement} ${closed}`).toBe(want);
        expect(item.todoRefinement).toBe(want === Status.SOMEDAY ? TodoRefinement.SOMEDAY : TodoRefinement.NEXT);
        expect(item.statusCategory).toBe(StatusCategory.TODO);
        expect(item.resolutionKind).toBeUndefined();
      }
    }
  });

  it("makes no instance without a rule", () => {
    const legacy = completed(daily, undefined);
    delete legacy.recurrence!.rule;
    expect(nextInstance(legacy, id(10), TODAY, ts(9_000))).toBeUndefined();
    delete legacy.recurrence;
    expect(nextInstance(legacy, id(10), TODAY, ts(9_000))).toBeUndefined();
  });
});
