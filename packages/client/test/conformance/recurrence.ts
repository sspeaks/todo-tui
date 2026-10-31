// Recurring Items (design 3.8) as a suite any daemon runs: capture with @,
// SetRecurrence, the next instance a completion creates (with the batch
// edge cases), undo and redo of both, request_id replays and
// ParseRecurrence. test/recurrence-conformance.test.ts runs it against
// FakeDaemon, uds.integration.test.ts against the real daemon ($TODO_BIN),
// spawned in the same zone. Mutations take today from the daemon's clock,
// so expected dates are counted from today in that zone, and the suite holds
// on any day; a test that fails after midnight there runs once more, on the
// new day. ParseRecurrence passes reference_time, so its today is fixed.
// The scheduler (the auto_done archive and healing) is FakeDaemon's alone
// here: the daemon's is tested in crates/todo-daemon/tests/server.rs.
import type { MessageInitShape } from "@bufbuild/protobuf";
import type { Timestamp } from "@bufbuild/protobuf/wkt";
import { Code, ConnectError } from "@connectrpc/connect";
import { afterEach, describe, expect, it as vitestIt } from "vitest";
import { civil, dayIn, dayNumber, dayOf, formatDay, weekday } from "../../src/dates.js";
import { BadRequestSchema, ErrorInfoSchema } from "../../src/gen/google/rpc/error_details_pb.js";
import {
  ArchiveReason,
  CreatedVia,
  type Item,
  Priority,
  type RecurrenceRule,
  type RecurrenceRuleSchema,
  Status,
  TodoRefinement,
  Weekday,
} from "../../src/gen/todo/v1/item_pb.js";
import { CaptureTokenKind } from "../../src/gen/todo/v1/item_service_pb.js";
import { OpKind } from "../../src/gen/todo/v1/op_pb.js";
import { RelationKind } from "../../src/gen/todo/v1/relation_pb.js";
import {
  type ItemsClient,
  addRelation,
  archiveItem,
  batchMutate,
  deleteItem,
  getUndoState,
  redo,
  setRecurrence,
  setStatus,
  undeleteItem,
  undo,
  updateItem,
} from "../../src/mutations.js";
import { parseDate, parseRecurrence } from "../../src/queries.js";
import { ulid } from "../../src/ulid.js";
import { localTime } from "./filter-views.js";

/** The zone both targets run the suite in: the real daemon is spawned with TZ set to it. */
export const RECURRENCE_ZONE = "Asia/Kathmandu";

/** A daemon in a known zone with an empty undo stack, for one test. */
export interface RecurrenceTarget {
  items: ItemsClient;
  /** The daemon's zone (an IANA name), which its today is taken in. */
  timeZone: string;
  /** The daemon's clock, in epoch milliseconds, if it is not Date.now. */
  now?: () => number;
  close?: () => void | Promise<void>;
}

type RuleInit = MessageInitShape<typeof RecurrenceRuleSchema>;

interface Started {
  zone: string;
  now: () => number;
  day: number;
}

const daily: RuleInit = { kind: { case: "daily", value: {} } };
const weekly = (...days: Weekday[]): RuleInit => ({ kind: { case: "weekly", value: { days } } });
const monthly = (day: number): RuleInit => ({ kind: { case: "monthly", value: { day } } });
const after = (days: number): RuleInit => ({ kind: { case: "afterCompletion", value: { days } } });

const DAY_NAMES = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/** A rule in a form to compare: `daily`, `weekly mon,thu`, `monthly 15`, `after 3`. */
function ruleText(r: RecurrenceRule | undefined): string | undefined {
  switch (r?.kind.case) {
    case undefined:
      return undefined;
    case "daily":
      return "daily";
    case "weekly":
      return `weekly ${r.kind.value.days.map((d) => DAY_NAMES[d - 1] ?? String(d)).join(",")}`;
    case "monthly":
      return `monthly ${r.kind.value.day}`;
    case "afterCompletion":
      return `after ${r.kind.value.days}`;
  }
}

const iso = (d: { year: number; month: number; day: number } | undefined) => (d === undefined ? undefined : formatDay(dayOf(d)));

/** An Item's recurrence as [rule, occurrence_date], with its due date and defer. */
const repeats = (item: Item | undefined) => ({
  rule: ruleText(item?.recurrence?.rule),
  occurrence: iso(item?.recurrence?.occurrenceDate),
  due: iso(item?.dueDate),
  defer: iso(item?.deferUntil),
});

/** The last day of the month `n` is in. */
function lastOfMonth(n: number): number {
  const { year, month } = civil(n);
  return month === 12 ? dayNumber(year + 1, 1, 1) - 1 : dayNumber(year, month + 1, 1) - 1;
}

/** The first date on or after `from` that is a scheduled `days` weekday (1 Monday … 7 Sunday). */
function firstWeekly(days: number[], from: number): number {
  for (let n = from; ; n++) if (days.includes(weekday(n))) return n;
}

/** The first date on or after `from` on `day` of the month, clamped to the month's last day. */
function firstMonthly(day: number, from: number): number {
  const inMonth = (first: number) => Math.min(first + day - 1, lastOfMonth(first));
  const { year, month } = civil(from);
  const here = inMonth(dayNumber(year, month, 1));
  return here >= from ? here : inMonth(lastOfMonth(from) + 1);
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/** `Mon Nov 2` as an op summary names a next instance's date, with the year when it is not today's. */
function summaryDay(n: number, today: number): string {
  const { year, month, day } = civil(n);
  const text = `${WEEKDAYS[weekday(n) - 1]} ${MONTHS[month - 1]} ${day}`;
  return year === civil(today).year ? text : `${text} ${year}`;
}

const ULID = /^[0-9A-HJKMNP-TV-Z]{26}$/;

const failure = (p: Promise<unknown>) => p.then(() => undefined, (e: unknown) => ConnectError.from(e));

/** The ErrorInfo reason, metadata and message of a failure with `code`. */
async function info(p: Promise<unknown>, code: Code) {
  const err = await failure(p);
  expect(err?.code).toBe(code);
  const detail = err?.findDetails(ErrorInfoSchema)[0];
  return { reason: detail?.reason, metadata: detail?.metadata, message: err?.rawMessage };
}

/** The [field, description] of an INVALID_ARGUMENT failure's single BadRequest violation. */
async function rejected(call: Promise<unknown>): Promise<[string | undefined, string | undefined]> {
  const err = await failure(call);
  expect(err?.code).toBe(Code.InvalidArgument);
  const [bad] = err?.findDetails(BadRequestSchema) ?? [];
  expect(bad?.fieldViolations).toHaveLength(1);
  return [bad?.fieldViolations[0]?.field, bad?.fieldViolations[0]?.description];
}

const status = (id: string, s: Status) => ({ kind: { case: "setStatus" as const, value: { id, status: s } } });
const repeat = (id: string, rule?: RuleInit) => ({ kind: { case: "setRecurrence" as const, value: { id, ...(rule && { rule }) } } });
const remove = (id: string) => ({ kind: { case: "deleteItem" as const, value: { id } } });

/** Registers the suite; `open` gives each test a fresh daemon. */
export function recurrenceConformance(open: () => Promise<RecurrenceTarget>): void {
  const cleanups: (() => void | Promise<void>)[] = [];
  const cleanUp = async () => {
    for (const c of cleanups.splice(0).reverse()) await c();
  };
  afterEach(cleanUp);

  /** The target's zone, clock and today when the running test set up. */
  let started: Started | undefined;

  /**
   * A test, run once more if it fails after midnight in the target's zone:
   * it counts its dates from the today it read, but the daemon reads its
   * own on each request.
   */
  function it(name: string, body: () => Promise<void>) {
    vitestIt(name, async () => {
      for (let attempt = 0; ; attempt++) {
        started = undefined;
        try {
          return await body();
        } catch (e) {
          // setup() sets it, which the compiler does not see.
          const s = started as Started | undefined;
          if (attempt > 0 || s === undefined || dayIn(s.zone, s.now()) === s.day) throw e;
          await cleanUp();
        }
      }
    });
  }

  async function setup() {
    const t = await open();
    if (t.close) cleanups.push(t.close);
    const now = t.now ?? Date.now;
    started = { zone: t.timeZone, now, day: dayIn(t.timeZone, now()) };
    const items = t.items;
    const capture = async (text: string, extra: { parentId?: string; referenceTime?: Timestamp } = {}) => {
      const res = await items.captureItem({ requestId: ulid(), text, createdVia: CreatedVia.TUI, ...extra });
      return { item: res.item!, draft: res.draft!, op: res.result!.opId };
    };
    const get = async (id: string) => (await items.getItem({ id })).item!;
    const top = async () => (await getUndoState(items)).undo;
    const revision = async () => (await items.listItems({ pageSize: 1 })).revision;
    /** Today in the daemon's zone, as a day number. */
    const today = () => dayIn(t.timeZone, now());
    /** A ^ date as the daemon reads it today. */
    const dateOf = async (expr: string) => dayOf((await parseDate(items, expr)).date!);
    return { t, items, capture, get, top, revision, today, dateOf };
  }

  describe("recurrence", () => {
    it("captures each @ rule as a new series, due on its first date", async () => {
      const { capture, get, today } = await setup();
      const d = today();
      const day = (n: number) => formatDay(n);
      const rows: [string, string, number | undefined][] = [
        ["@daily", "daily", d],
        ["@WEEKDAYS", "weekly mon,tue,wed,thu,fri", firstWeekly([1, 2, 3, 4, 5], d)],
        ["@weekly", `weekly ${DAY_NAMES[weekday(d) - 1]}`, d],
        ["@mon,thu", "weekly mon,thu", firstWeekly([1, 4], d)],
        ["@thu,mon,thursday", "weekly mon,thu", firstWeekly([1, 4], d)],
        ["@monthly", `monthly ${civil(d).day}`, d],
        ["@15th", "monthly 15", firstMonthly(15, d)],
        ["@31st", "monthly 31", firstMonthly(31, d)],
        // After completion: no first date without a due date.
        ["@3d", "after 3", undefined],
        ["@2w", "after 14", undefined],
      ];
      const series = new Set<string>();
      for (const [token, rule, first] of rows) {
        const { item, draft } = await capture(`water plants ${token} +2`);
        const want = { rule, occurrence: first === undefined ? undefined : day(first), due: first === undefined ? undefined : day(first), defer: undefined };
        expect(item.title, token).toBe("water plants");
        expect(item.priority, token).toBe(Priority.HIGH);
        expect(repeats(item), token).toEqual(want);
        expect(item.recurrence?.seriesId, token).toMatch(ULID);
        series.add(item.recurrence!.seriesId);
        expect([ruleText(draft.recurrence), iso(draft.recurrenceFirst), iso(draft.dueDate)], token).toEqual([rule, want.occurrence, undefined]);
        expect(draft.tokens.map((tk) => [tk.start, tk.end, tk.kind, tk.applied]), token).toEqual([
          [13, 13 + token.length, CaptureTokenKind.RECURRENCE, true],
          [14 + token.length, 16 + token.length, CaptureTokenKind.PRIORITY, true],
        ]);
        expect(item.createdVia).toBe(CreatedVia.TUI);
        expect(await get(item.id)).toEqual(item);
      }
      // Every capture starts its own series.
      expect(series.size).toBe(rows.length);
    });

    it("anchors @weekly and @monthly on the ^ date wherever it is, and keeps that due date", async () => {
      const { capture, dateOf } = await setup();
      const fri = await dateOf("fri");
      for (const text of ["pay @weekly ^fri", "pay ^fri @weekly", "@weekly pay ^fri"]) {
        const { item, draft } = await capture(text);
        expect(repeats(item), text).toEqual({ rule: "weekly fri", occurrence: formatDay(fri), due: formatDay(fri), defer: undefined });
        expect(iso(draft.dueDate), text).toBe(formatDay(fri));
      }
      const eom = await dateOf("eom");
      const { item: rent } = await capture("rent ^eom @monthly");
      expect(repeats(rent)).toEqual({ rule: `monthly ${civil(eom).day}`, occurrence: formatDay(eom), due: formatDay(eom), defer: undefined });
      // A due date off the schedule is kept; the slot is the first scheduled date on or after it.
      const { item: off } = await capture("call ^fri @mon,wed");
      expect(repeats(off)).toEqual({ rule: "weekly mon,wed", occurrence: formatDay(firstWeekly([1, 3], fri)), due: formatDay(fri), defer: undefined });
      // After completion: the slot is the due date.
      const { item: later } = await capture("water @2w ^fri");
      expect(repeats(later)).toEqual({ rule: "after 14", occurrence: formatDay(fri), due: formatDay(fri), defer: undefined });
    });

    it("keeps an @ that is not a rule in the title, with a warning, and stores no recurrence", async () => {
      const { capture } = await setup();
      const rows: [string, string, string[]][] = [
        ["work @home", "work @home", ["@home: not a repeat rule"]],
        ["x @22th", "x @22th", ["@22th: write 22nd"]],
        ["x @0d", "x @0d", ["@0d: after completion must be 1 to 3650 days"]],
        ["x @mon,fr", "x @mon,fr", ["@mon,fr: fr is not a weekday"]],
        ["\\@daily standup", "@daily standup", []],
        ["mail bob@x.com", "mail bob@x.com", []],
        ["meet @ noon", "meet @ noon", []],
      ];
      for (const [text, title, warnings] of rows) {
        const { item, draft } = await capture(text);
        expect([item.title, item.recurrence, item.dueDate], text).toEqual([title, undefined, undefined]);
        expect([draft.warnings, draft.recurrence, draft.recurrenceFirst], text).toEqual([warnings, undefined, undefined]);
      }
      // A later rule overrides an earlier one.
      const { item } = await capture("x @daily @3d");
      expect(repeats(item).rule).toBe("after 3");
    });

    it("SetRecurrence sets a rule, replaces it in the same series and clears it, each an undo entry", async () => {
      const { items, capture, top, today } = await setup();
      const d = today();
      const x = (await capture("water plants")).item;

      const set = await setRecurrence(items, x.id, daily, { expectedRevision: x.revision });
      const series = set.item!.recurrence!.seriesId;
      expect(series).toMatch(ULID);
      // Without a due date the first occurrence is today, which becomes it.
      expect(repeats(set.item)).toEqual({ rule: "daily", occurrence: formatDay(d), due: formatDay(d), defer: undefined });
      expect(set.item?.revision).toBe(set.result?.revision);
      expect(await top()).toMatchObject({ opId: set.result?.opId, kind: OpKind.SET_RECURRENCE, itemIds: [x.id], description: 'set repeat "water plants"' });

      // A new rule keeps the series and the due date, and moves the slot.
      // Weekly days may come in any order and repeat.
      const changed = await setRecurrence(items, x.id, weekly(Weekday.THURSDAY, Weekday.MONDAY, Weekday.THURSDAY));
      expect(changed.item?.recurrence?.seriesId).toBe(series);
      expect(repeats(changed.item)).toEqual({ rule: "weekly mon,thu", occurrence: formatDay(firstWeekly([1, 4], d)), due: formatDay(d), defer: undefined });

      // After completion: the slot is the due date, which stays.
      const later = await setRecurrence(items, x.id, after(3));
      expect(repeats(later.item)).toEqual({ rule: "after 3", occurrence: formatDay(d), due: formatDay(d), defer: undefined });

      // The same rule again changes nothing and is no entry.
      const same = await setRecurrence(items, x.id, after(3));
      expect(same.result?.revision).toBe(later.result?.revision);
      expect((await top())?.opId).toBe(later.result?.opId);

      // No rule clears it; the Item stays, due as it was.
      const cleared = await setRecurrence(items, x.id, undefined);
      expect(repeats(cleared.item)).toEqual({ rule: undefined, occurrence: undefined, due: formatDay(d), defer: undefined });
      expect(cleared.item?.deletedAt).toBeUndefined();
      expect((await top())?.description).toBe('stop repeating "water plants"');

      // Undo brings each back in turn; the due date set with the first rule goes with it.
      expect(repeats((await undo(items)).items[0])).toEqual(repeats(later.item));
      await undo(items);
      await undo(items);
      const first = (await undo(items)).items[0];
      expect(repeats(first)).toEqual({ rule: undefined, occurrence: undefined, due: undefined, defer: undefined });
      // Redo starts the same series again.
      expect((await redo(items)).items[0]?.recurrence?.seriesId).toBe(series);
    });

    it("SetRecurrence keeps a due date you set, and an after-completion rule needs none", async () => {
      const { items, capture, dateOf, today } = await setup();
      const tom = await dateOf("tom");
      const due = (await capture("pay ^tom")).item;
      const fixed = await setRecurrence(items, due.id, monthly(civil(today()).day));
      // Monthly on today's day, counted from tomorrow: next month.
      const slot = firstMonthly(civil(today()).day, tom);
      expect(repeats(fixed.item)).toEqual({ rule: `monthly ${civil(today()).day}`, occurrence: formatDay(slot), due: formatDay(tom), defer: undefined });

      const none = (await capture("stretch")).item;
      const res = await setRecurrence(items, none.id, after(2));
      expect(repeats(res.item)).toEqual({ rule: "after 2", occurrence: undefined, due: undefined, defer: undefined });
    });

    it("SetRecurrence refuses a rule on a closed Item, a missing or deleted one, and invalid rules", async () => {
      const { items, capture, get, revision } = await setup();
      const x = (await capture("x")).item;
      const rev = await revision();
      const rows: [RuleInit, string, string][] = [
        [{}, "rule.kind", "required field is missing"],
        [weekly(), "rule.weekly.days", "a weekly rule needs at least one day"],
        [weekly(Weekday.UNSPECIFIED), "rule.weekly.days", "value must not be UNSPECIFIED"],
        [weekly(99 as Weekday), "rule.weekly.days", "unknown enum value 99"],
        [monthly(0), "rule.monthly.day", "the day of the month must be 1 to 31"],
        [monthly(32), "rule.monthly.day", "the day of the month must be 1 to 31"],
        [after(0), "rule.after_completion.days", "after completion must be 1 to 3650 days"],
        [after(3651), "rule.after_completion.days", "after completion must be 1 to 3650 days"],
      ];
      for (const [rule, field, description] of rows) {
        expect(await rejected(setRecurrence(items, x.id, rule)), field).toEqual([field, description]);
        expect(await rejected(batchMutate(items, [repeat(x.id, rule)])), field).toEqual([`mutations[0].set_recurrence.${field}`, description]);
      }
      expect(await rejected(setRecurrence(items, "nope", daily))).toEqual(["id", 'invalid ULID "nope"']);
      expect(await rejected(setRecurrence(items, x.id, daily, { requestId: "nope" }))).toEqual(["request_id", "must be a ULID"]);
      const nested = batchMutate(items, [{ kind: { case: "setRecurrence", value: { id: x.id, rule: daily, requestId: ulid() } } }]);
      expect(await rejected(nested)).toEqual(["mutations[0].set_recurrence.request_id", "must be empty in a batch: the batch's request_id covers it"]);
      expect(await revision()).toBe(rev);
      expect((await get(x.id)).recurrence).toBeUndefined();

      const missing = ulid();
      expect(await info(setRecurrence(items, missing, daily), Code.NotFound)).toMatchObject({ reason: "ITEM_NOT_FOUND", metadata: { item_id: missing } });
      const gone = (await capture("gone")).item;
      await deleteItem(items, gone.id);
      expect(await info(setRecurrence(items, gone.id, daily), Code.FailedPrecondition)).toMatchObject({ reason: "ITEM_DELETED", metadata: { item_id: gone.id } });

      // A done or cancelled Item takes no rule: reopen it first.
      for (const closed of [Status.DONE, Status.CANCELLED]) {
        const y = (await capture("y")).item;
        await setStatus(items, y.id, closed);
        expect(await info(setRecurrence(items, y.id, daily), Code.FailedPrecondition)).toEqual({
          reason: "ITEM_CLOSED",
          metadata: { item_id: y.id },
          message: `item ${y.id} is done or cancelled: reopen it first`,
        });
        expect(await info(batchMutate(items, [repeat(y.id, daily)]), Code.FailedPrecondition)).toEqual({
          reason: "ITEM_CLOSED",
          metadata: { item_id: y.id },
          message: `mutations[0].set_recurrence.id: item ${y.id} is done or cancelled: reopen it first`,
        });
        // Reopened in the same batch, it may.
        const reopened = await batchMutate(items, [status(y.id, Status.NEXT), repeat(y.id, daily)]);
        expect(repeats(reopened.items[1]).rule).toBe("daily");
      }
      // ... but a closed Item may lose its rule.
      const z = (await capture("z @daily")).item;
      await setStatus(items, z.id, Status.DONE);
      expect((await setRecurrence(items, z.id, undefined)).item?.recurrence).toBeUndefined();
    });

    it("SetRecurrence checks expected_revision and replays by request_id", async () => {
      const { items, capture, get, revision } = await setup();
      const x = (await capture("x")).item;
      const stale = await info(setRecurrence(items, x.id, daily, { expectedRevision: x.revision + 1n }), Code.Aborted);
      expect(stale.reason).toBe("REVISION_MISMATCH");
      expect((await get(x.id)).recurrence).toBeUndefined();

      const requestId = ulid();
      const first = await setRecurrence(items, x.id, daily, { requestId, expectedRevision: x.revision });
      const rev = await revision();
      // A replay answers the original, though its expected_revision is stale by now.
      const again = await setRecurrence(items, x.id, daily, { requestId, expectedRevision: x.revision });
      expect([again.item, again.result]).toEqual([first.item, first.result]);
      expect(await revision()).toBe(rev);
      const other = (await capture("other")).item;
      expect(await rejected(setRecurrence(items, other.id, daily, { requestId }))).toEqual(["request_id", expect.any(String)]);
    });

    it("a bulk SetRecurrence gives each Item its own series; an Item that recurs keeps its own", async () => {
      const { items, capture, top } = await setup();
      const [a, b, c] = [(await capture("a")).item, (await capture("b")).item, (await capture("c @daily")).item];
      const res = await batchMutate(items, [repeat(a.id, daily), repeat(b.id, daily), repeat(c.id, weekly(Weekday.MONDAY))]);
      const series = res.items.map((i) => i.recurrence?.seriesId);
      expect(new Set(series).size).toBe(3);
      expect(series[2]).toBe(c.recurrence?.seriesId);
      expect(res.generated).toEqual([]);
      expect(await top()).toMatchObject({ kind: OpKind.BATCH, description: "set repeat (3 items)" });
      await batchMutate(items, [repeat(a.id), repeat(b.id)]);
      expect((await top())?.description).toBe("stop repeating (2 items)");
    });

    it("marking an instance done creates the next, copying what the plan lists", async () => {
      const { items, capture, get, top, today } = await setup();
      const d = today();
      const garden = (await capture("garden")).item;
      const other = (await capture("other")).item;
      const x = (await capture("water plants @daily +2 #home #garden", { parentId: garden.id })).item;
      await updateItem(
        items,
        {
          id: x.id,
          description: "the ferns too",
          itemType: "chore",
          localPriorityOverride: Priority.LOW,
          originalEstimate: { seconds: 1800n },
          remainingEstimate: { seconds: 600n },
          waiting: { on: { ref: { case: "freeText", value: "dana" } } },
          privateNotes: "notes",
          focusWeek: "2026-W43",
        },
        ["description", "item_type", "local_priority_override", "original_estimate", "remaining_estimate", "waiting", "private_notes", "focus_week"],
      );
      await addRelation(items, { fromId: x.id, toId: other.id, kind: RelationKind.BLOCKS });
      await archiveItem(items, x.id);

      const res = await setStatus(items, x.id, Status.DONE);
      const done = res.item!;
      const next = res.generated!;
      expect(done.status).toBe(Status.DONE);
      expect(next.id).toMatch(ULID);
      expect(next.id).not.toBe(x.id);
      expect(next).toMatchObject({
        title: "water plants",
        description: "the ferns too",
        itemType: "chore",
        priority: Priority.HIGH,
        localPriorityOverride: Priority.LOW,
        labels: ["home", "garden"],
        parentId: garden.id,
        originalEstimate: { seconds: 1800n },
        remainingEstimate: { seconds: 600n },
        createdVia: CreatedVia.RECURRENCE,
        status: Status.NEXT,
        todoRefinement: TodoRefinement.NEXT,
        privateNotes: "",
        relations: [],
        parentTitle: "garden",
      });
      expect([next.waiting, next.focusWeek, next.archived, next.resolutionKind, next.deletedAt]).toEqual([undefined, undefined, undefined, undefined, undefined]);
      // Due today, completed today: tomorrow, hidden until then.
      expect(next.recurrence?.seriesId).toBe(x.recurrence?.seriesId);
      expect(repeats(next)).toEqual({ rule: "daily", occurrence: formatDay(d + 1), due: formatDay(d + 1), defer: formatDay(d + 1) });
      // One op: the completed Item, the new one, then their parent, re-emitted
      // for its subtask counts.
      expect(next.revision).toBe(done.revision + 1n);
      expect(await get(next.id)).toEqual(next);
      const parent = await get(garden.id);
      expect([parent.subtasksDone, parent.subtasksTotal, parent.revision]).toEqual([1, 2, res.result?.revision]);
      expect(await top()).toMatchObject({
        opId: res.result?.opId,
        kind: OpKind.SET_STATUS,
        itemIds: [x.id, next.id],
        description: `mark done "water plants" · next ${summaryDay(d + 1, d)}`,
      });

      // An Item that does not recur creates nothing.
      expect((await setStatus(items, other.id, Status.DONE)).generated).toBeUndefined();
    });

    it("schedules the next date by the rule: missed dates skipped, today not deferred", async () => {
      const { items, capture, today } = await setup();
      const d = today();
      const next = async (text: string) => {
        const { item } = await capture(text);
        return repeats((await setStatus(items, item.id, Status.DONE)).generated);
      };
      const on = (n: number, rule: string) => ({ rule, occurrence: formatDay(n), due: formatDay(n), defer: n > d ? formatDay(n) : undefined });
      // Due a week ago and completed today: today is the first slot not missed, and is not deferred.
      expect(await next(`late ^${formatDay(d - 7)} @daily`)).toEqual(on(d, "daily"));
      expect(await next(`late ^${formatDay(d - 7)} @weekly`)).toEqual(on(d, `weekly ${DAY_NAMES[weekday(d) - 1]}`));
      // Due today: the next scheduled date after it.
      expect(await next("x @weekly")).toEqual(on(d + 7, `weekly ${DAY_NAMES[weekday(d) - 1]}`));
      expect(await next("x @mon,thu")).toEqual(on(firstWeekly([1, 4], firstWeekly([1, 4], d) + 1), "weekly mon,thu"));
      const day = civil(d).day;
      expect(await next("x @monthly")).toEqual(on(firstMonthly(day, lastOfMonth(d) + 1), `monthly ${day}`));
      // After completion counts from today, whatever the due date.
      expect(await next("x @3d")).toEqual(on(d + 3, "after 3"));
      expect(await next(`x @2w ^${formatDay(d + 30)}`)).toEqual(on(d + 14, "after 14"));
    });

    it("cancelling an instance creates the next too; its status follows the refinement", async () => {
      const { items, capture, top, today } = await setup();
      const d = today();
      const rows: [string, Status | undefined, Status, Status][] = [
        ["inbox @daily", undefined, Status.CANCELLED, Status.NEXT],
        ["next @daily >next", undefined, Status.DONE, Status.NEXT],
        ["someday @daily >someday", undefined, Status.CANCELLED, Status.SOMEDAY],
        ["started @daily >someday", Status.IN_PROGRESS, Status.DONE, Status.SOMEDAY],
      ];
      for (const [text, first, closed, want] of rows) {
        const { item } = await capture(text);
        if (first !== undefined) await setStatus(items, item.id, first);
        const res = await setStatus(items, item.id, closed);
        expect([res.generated?.status, res.generated?.resolutionKind], text).toEqual([want, undefined]);
      }
      expect((await top())?.description).toBe(`mark done "started" · next ${summaryDay(d + 1, d)}`);
      const { item } = await capture("skip me @daily");
      await setStatus(items, item.id, Status.CANCELLED);
      expect((await top())?.description).toBe(`cancel "skip me" · next ${summaryDay(d + 1, d)}`);
    });

    it("creates nothing for done → cancelled, or while another instance of the series is open", async () => {
      const { items, capture, revision } = await setup();
      const a = (await capture("a @daily")).item;
      const b = (await setStatus(items, a.id, Status.DONE)).generated!;
      // Reopened, a is one of two open instances: completing it creates nothing.
      await setStatus(items, a.id, Status.NEXT);
      expect((await setStatus(items, a.id, Status.DONE)).generated).toBeUndefined();
      // With b gone, done → cancelled is no completion either.
      await deleteItem(items, b.id);
      expect((await setStatus(items, a.id, Status.CANCELLED)).generated).toBeUndefined();
      // Nor is a status change between open ones.
      const c = (await capture("c @daily")).item;
      const rev = await revision();
      expect((await setStatus(items, c.id, Status.IN_PROGRESS)).generated).toBeUndefined();
      // One change of c, and no create.
      expect(await revision()).toBe(rev + 1n);
    });

    it("decides a batch by the state it leaves: reopened, deleted or cleared Items create nothing", async () => {
      const { items, capture, get, revision } = await setup();
      for (const [name, then] of [
        ["reopened", (id: string) => status(id, Status.NEXT)],
        ["deleted", (id: string) => remove(id)],
        ["cleared", (id: string) => repeat(id)],
      ] as const) {
        const a = (await capture(`${name} @daily`)).item;
        const rev = await revision();
        const res = await batchMutate(items, [status(a.id, Status.DONE), then(a.id)]);
        expect(res.generated, name).toEqual([]);
        // Two changes of one Item, and no create.
        expect(res.result?.revision, name).toBe(rev + 1n);
        expect(res.items.map((i) => i.id)).toEqual([a.id, a.id]);
      }
      // Cleared first, then done: nothing either.
      const b = (await capture("b @daily")).item;
      expect((await batchMutate(items, [repeat(b.id), status(b.id, Status.DONE)])).generated).toEqual([]);
      // Set in the batch, then done: the new rule's series goes on.
      const c = (await capture("c")).item;
      const set = await batchMutate(items, [repeat(c.id, daily), status(c.id, Status.DONE)]);
      expect(set.generated.map((g) => g.title)).toEqual(["c"]);
      expect(set.generated[0]?.recurrence?.seriesId).toBe(set.items[0]?.recurrence?.seriesId);
      expect((await get(c.id)).status).toBe(Status.DONE);
    });

    it("completes two instances of one series in one batch with a single next instance", async () => {
      const { items, capture, top, today } = await setup();
      const d = today();
      const a = (await capture("water @daily")).item;
      const b = (await setStatus(items, a.id, Status.DONE)).generated!;
      await setStatus(items, a.id, Status.NEXT);
      // Both open now; closing both leaves one open instance, made from the first.
      const res = await batchMutate(items, [status(a.id, Status.DONE), status(b.id, Status.DONE)]);
      expect(res.generated).toHaveLength(1);
      const c = res.generated[0]!;
      expect(c.recurrence?.seriesId).toBe(a.recurrence?.seriesId);
      expect([c.id === a.id, c.id === b.id]).toEqual([false, false]);
      expect(repeats(c)).toEqual({ rule: "daily", occurrence: formatDay(d + 1), due: formatDay(d + 1), defer: formatDay(d + 1) });
      expect(await top()).toMatchObject({ kind: OpKind.BATCH, itemIds: [a.id, b.id, c.id], description: "mark done (2 items) · 1 next" });
    });

    it("BatchMutate returns the next instances in mutation order, and summarises them", async () => {
      const { items, capture, top, today, revision } = await setup();
      const d = today();
      const a = (await capture("a @daily")).item;
      const b = (await capture("b @3d")).item;
      const c = (await capture("c")).item;
      const requestId = ulid();
      const res = await batchMutate(items, [status(b.id, Status.DONE), status(c.id, Status.DONE), status(a.id, Status.DONE)], { requestId });
      expect(res.generated.map((g) => [g.title, g.recurrence?.seriesId, iso(g.dueDate)])).toEqual([
        ["b", b.recurrence?.seriesId, formatDay(d + 3)],
        ["a", a.recurrence?.seriesId, formatDay(d + 1)],
      ]);
      expect(res.items.map((i) => i.status)).toEqual([Status.DONE, Status.DONE, Status.DONE]);
      expect(await top()).toMatchObject({ opId: res.result?.opId, kind: OpKind.BATCH, description: "mark done (3 items) · 2 next" });

      // A replay answers the same instances and creates no more.
      const rev = await revision();
      const replay = await batchMutate(items, [status(b.id, Status.DONE), status(c.id, Status.DONE), status(a.id, Status.DONE)], { requestId });
      expect(replay.result).toEqual(res.result);
      expect(replay.generated).toEqual(res.generated);
      expect(await revision()).toBe(rev);

      // Mixed statuses read as a change.
      const e = (await capture("e @daily")).item;
      const f = (await capture("f @daily")).item;
      await batchMutate(items, [status(e.id, Status.DONE), status(f.id, Status.CANCELLED)]);
      expect((await top())?.description).toBe("change 2 items · 2 next");
      // One Item with one next names its date.
      const g = (await capture("g @daily")).item;
      await batchMutate(items, [status(g.id, Status.DONE)]);
      expect((await top())?.description).toBe(`mark done "g" · next ${summaryDay(d + 1, d)}`);
    });

    it("SetStatus replays by request_id with the same next instance, creating no other", async () => {
      const { items, capture, revision } = await setup();
      const a = (await capture("a @daily")).item;
      const requestId = ulid();
      const first = await setStatus(items, a.id, Status.DONE, { requestId });
      const rev = await revision();
      const again = await setStatus(items, a.id, Status.DONE, { requestId });
      expect([again.result, again.generated]).toEqual([first.result, first.generated]);
      expect(await revision()).toBe(rev);
    });

    it("undo removes the next instance and reopens the completed one; redo brings both back", async () => {
      const { items, capture, get, top } = await setup();
      const a = (await capture("water @daily")).item;
      const done = await setStatus(items, a.id, Status.DONE);
      const b = done.generated!;
      const entry = await top();

      const undone = await undo(items, { expectedOpId: done.result!.opId });
      expect(undone.entry).toEqual(entry);
      expect(undone.skipped).toEqual([]);
      expect(undone.items.map((i) => i.id).sort()).toEqual([a.id, b.id].sort());
      expect((await get(a.id)).status).toBe(Status.INBOX);
      expect((await get(b.id)).deletedAt).toBeDefined();
      expect((await top())?.opId).not.toBe(done.result?.opId);

      const redone = await redo(items, { expectedOpId: done.result!.opId });
      expect(redone.skipped).toEqual([]);
      expect((await get(a.id)).status).toBe(Status.DONE);
      const back = await get(b.id);
      expect(back.deletedAt).toBeUndefined();
      expect(repeats(back)).toEqual(repeats(b));
      expect((await top())?.opId).toBe(done.result?.opId);

      // Undone again, a is the open instance: completing it again creates a new one.
      await undo(items);
      const again = await setStatus(items, a.id, Status.DONE);
      expect(again.generated?.id).not.toBe(b.id);
      expect(again.generated?.recurrence?.seriesId).toBe(a.recurrence?.seriesId);
    });

    it("deleting the open instance ends the series; undeleting it resumes", async () => {
      const { items, capture, get } = await setup();
      const a = (await capture("water @daily")).item;
      const b = (await setStatus(items, a.id, Status.DONE)).generated!;
      await deleteItem(items, b.id);
      expect((await get(b.id)).deletedAt).toBeDefined();
      // Nothing is open; the series resumes only when b is back.
      await undeleteItem(items, b.id);
      const c = (await setStatus(items, b.id, Status.DONE)).generated!;
      expect(c.recurrence?.seriesId).toBe(a.recurrence?.seriesId);
      expect([c.id === a.id, c.id === b.id]).toEqual([false, false]);
    });

    it("reopening an Item leaves a user archive alone", async () => {
      const { items, capture } = await setup();
      const a = (await capture("a")).item;
      await setStatus(items, a.id, Status.DONE);
      await archiveItem(items, a.id);
      const reopened = await setStatus(items, a.id, Status.NEXT);
      expect(reopened.item?.archived?.reason).toBe(ArchiveReason.USER);
    });

    it("UpdateItem refuses recurrence: read-only, and recurrence.rule is no path", async () => {
      const { items, capture } = await setup();
      const a = (await capture("a @daily")).item;
      expect(await rejected(updateItem(items, { id: a.id }, ["recurrence.rule"]))).toEqual(["update_mask", 'unknown field path "recurrence.rule"']);
      for (const path of ["recurrence", "recurrence.series_id", "recurrence.occurrence_date"]) {
        expect(await rejected(updateItem(items, { id: a.id, title: "b" }, ["title", path])), path).toEqual([`item.${path}`, "is read-only"]);
      }
    });
  });

  describe("ParseRecurrence", () => {
    /** Local 08:00 on a day of 2026 in the target's zone. */
    const at = (t: RecurrenceTarget, month: number, day: number) => localTime(t.timeZone, 2026, month, day, 8);

    it("resolves each rule form, its first date counted from the anchor, else today", async () => {
      const { t, items } = await setup();
      // Thursday 2026-10-22.
      const THU = at(t, 10, 22);
      const date = (s: string) => ({ year: Number(s.slice(0, 4)), month: Number(s.slice(5, 7)), day: Number(s.slice(8)) });
      const parse = async (text: string, anchor?: string, referenceTime: Timestamp = THU) => {
        const res = await parseRecurrence(items, text, { referenceTime, ...(anchor !== undefined && { anchor: date(anchor) }) });
        return [ruleText(res.rule), iso(res.first), res.warning];
      };
      const rows: [string, string | undefined, string, string | undefined][] = [
        ["daily", undefined, "daily", "2026-10-22"],
        ["@daily", "2026-10-30", "daily", "2026-10-30"],
        ["  @DAILY ", undefined, "daily", "2026-10-22"],
        ["@ daily", undefined, "daily", "2026-10-22"],
        ["weekdays", "2026-10-24", "weekly mon,tue,wed,thu,fri", "2026-10-26"],
        ["mon,thu", "2026-10-23", "weekly mon,thu", "2026-10-26"],
        ["thu,mon", undefined, "weekly mon,thu", "2026-10-22"],
        ["tues,thurs", undefined, "weekly tue,thu", "2026-10-22"],
        ["sun,sat,fri", undefined, "weekly fri,sat,sun", "2026-10-23"],
        // weekly and monthly take the anchor's weekday and day.
        ["weekly", undefined, "weekly thu", "2026-10-22"],
        ["@weekly", "2026-10-30", "weekly fri", "2026-10-30"],
        ["monthly", undefined, "monthly 22", "2026-10-22"],
        ["monthly", "2027-01-31", "monthly 31", "2027-01-31"],
        ["15th", undefined, "monthly 15", "2026-11-15"],
        ["1st", undefined, "monthly 1", "2026-11-01"],
        ["31st", "2026-11-05", "monthly 31", "2026-11-30"],
        ["29th", "2028-02-01", "monthly 29", "2028-02-29"],
        ["22nd", "2026-12-23", "monthly 22", "2027-01-22"],
        // After completion: the first is the anchor, if any.
        ["3d", undefined, "after 3", undefined],
        ["@2w", "2026-10-30", "after 14", "2026-10-30"],
        ["3650d", undefined, "after 3650", undefined],
      ];
      for (const [text, anchor, rule, first] of rows) expect(await parse(text, anchor), `${text} ${anchor}`).toEqual([rule, first, ""]);
      // Today is the reference day in the daemon's zone.
      expect(await parse("weekly", undefined, at(t, 10, 25))).toEqual(["weekly sun", "2026-10-25", ""]);
      expect(await parse("mon", undefined, at(t, 10, 27))).toEqual(["weekly mon", "2026-11-02", ""]);
    });

    it("words a rule that does not parse as ParseCapture words the @ token", async () => {
      const { t, items } = await setup();
      const referenceTime = at(t, 10, 22);
      const rows: [string, string][] = [
        ["home", "home: not a repeat rule"],
        ["@home", "@home: not a repeat rule"],
        ["  @home ", "@home: not a repeat rule"],
        ["@@daily", "@@daily: not a repeat rule"],
        ["22th", "22th: write 22nd"],
        ["32nd", "32nd: the day of the month must be 1st to 31st"],
        ["0d", "0d: after completion must be 1 to 3650 days"],
        ["522w", "522w: after completion must be 1 to 3650 days"],
        ["15", "15: a number needs d or w, or a day suffix, e.g. 3d, 2w or 15th"],
        ["mon,xyz", "mon,xyz: xyz is not a weekday"],
        ["mon,,thu", "mon,,thu: a day is missing between commas"],
        ["", "empty repeat rule"],
        ["   ", "empty repeat rule"],
        ["@", "@: empty repeat rule"],
      ];
      for (const [text, warning] of rows) {
        const res = await parseRecurrence(items, text, { referenceTime, anchor: { year: 2026, month: 10, day: 30 } });
        expect([res.rule, res.first, res.warning], JSON.stringify(text)).toEqual([undefined, undefined, warning]);
      }
      // The capture grammar says the same of the token.
      const draft = (await items.parseCapture({ text: "x @home", referenceTime })).draft;
      expect(draft?.warnings).toEqual(["@home: not a repeat rule"]);
    });

    it("checks the anchor, reference_time and the text's length", async () => {
      const { items } = await setup();
      expect((await rejected(parseRecurrence(items, "daily", { anchor: { year: 2026, month: 2, day: 30 } })))[0]).toBe("anchor");
      expect((await rejected(parseRecurrence(items, "daily", { anchor: { year: 0, month: 1, day: 1 } })))[0]).toBe("anchor");
      expect((await rejected(parseRecurrence(items, "daily", { referenceTime: { seconds: 2n ** 62n, nanos: 0 } as Timestamp })))[0]).toBe("reference_time");
      expect(await rejected(parseRecurrence(items, "x".repeat(64 * 1024 + 1)))).toEqual(["text", "is longer than 64 KiB"]);
    });
  });
}
