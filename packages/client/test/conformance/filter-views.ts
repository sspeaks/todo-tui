// The filter language, sorts and keyset paging, the default views, the
// counts and ParseDate (filter.proto, design 2.4, 4.8) as a suite any daemon
// runs: test/filter-views.test.ts runs it against FakeDaemon,
// uds.integration.test.ts against the real daemon ($TODO_BIN), spawned in the
// same zone. Every case passes reference_time, so "today" is fixed, and the
// Items are made only through public RPCs. The fixture is the one of
// m3_filter-hard-cases.md §10 minus what the API cannot write (remote links,
// assignees), on Sunday 2026-10-25, in ISO week 2026-W43.
import { type MessageInitShape, create, toJson } from "@bufbuild/protobuf";
import { type Timestamp, timestampFromDate } from "@bufbuild/protobuf/wkt";
import { Code, ConnectError, type Transport, createClient } from "@connectrpc/connect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { BadRequestSchema, ErrorInfoSchema } from "../../src/gen/google/rpc/error_details_pb.js";
import { Topic } from "../../src/gen/todo/v1/common_pb.js";
import { ItemField } from "../../src/gen/todo/v1/field_pb.js";
import {
  type ClauseSchema,
  type FilterSchema,
  type OperandSchema,
  Operator,
  RelativeDate_Anchor as Anchor,
  type SortKeySchema,
} from "../../src/gen/todo/v1/filter_pb.js";
import { CreatedVia, type ItemSchema, Priority, Status } from "../../src/gen/todo/v1/item_pb.js";
import type { ListItemsRequestSchema } from "../../src/gen/todo/v1/item_service_pb.js";
import { type View, ViewSchema } from "../../src/gen/todo/v1/view_pb.js";
import { ViewService } from "../../src/gen/todo/v1/view_service_pb.js";
import { WatchService } from "../../src/gen/todo/v1/watch_service_pb.js";
import { defaultViews } from "../../src/default-views.js";
import { type ItemsClient, archiveItem, deleteItem, setStatus, updateItem } from "../../src/mutations.js";
import { getStatusCounts, listViews, parseDate } from "../../src/queries.js";
import { ulid } from "../../src/ulid.js";

/** The zone both targets run the suite in: the real daemon is spawned with TZ set to it. */
export const FILTER_ZONE = "America/New_York";

/** A fresh daemon (no Items yet; the default views seeded) in a known zone. */
export interface FilterTarget {
  items: ItemsClient;
  /** For a ViewService client on the same daemon. */
  transport: Transport;
  /** The daemon's zone (an IANA name), which its today is taken in. */
  timeZone: string;
  close?: () => void | Promise<void>;
}

type ClauseInit = MessageInitShape<typeof ClauseSchema>;
type OperandInit = MessageInitShape<typeof OperandSchema>;
type FilterInit = MessageInitShape<typeof FilterSchema>;
type SortInit = MessageInitShape<typeof SortKeySchema>;
type ListInit = MessageInitShape<typeof ListItemsRequestSchema>;
type ValueInit = NonNullable<Extract<OperandInit["kind"], { case: "value" }>["value"]>["kind"];

const F = ItemField;
const O = Operator;

const p = (target: ItemField, op: Operator, ...operands: OperandInit[]): ClauseInit => ({ kind: { case: "predicate", value: { target, op, operands } } });
const group = (allOf: ClauseInit[], anyOf: ClauseInit[] = []): ClauseInit => ({ kind: { case: "group", value: { allOf, anyOf } } });
const value = (kind: ValueInit): OperandInit => ({ kind: { case: "value", value: { kind } } });
const status = (s: Status) => value({ case: "status", value: s });
const str = (s: string) => value({ case: "stringValue", value: s });
const prio = (v: Priority) => value({ case: "priority", value: v });
const free = (text: string) => value({ case: "person", value: { ref: { case: "freeText", value: text } } });
const rel = (anchor: Anchor, offsetDays = 0): OperandInit => ({ kind: { case: "relativeDate", value: { anchor, offsetDays } } });
const text = (t: string): OperandInit => ({ kind: { case: "text", value: t } });
const asc = (key: ItemField): SortInit => ({ key });
const desc = (key: ItemField): SortInit => ({ key, descending: true });
const date = (year: number, month: number, day: number) => ({ year, month, day });

/** The instant of a wall-clock time in `zone` (not one a DST change skips or repeats). */
export function localTime(zone: string, y: number, m: number, d: number, h = 12, mi = 0, s = 0): Timestamp {
  const format = new Intl.DateTimeFormat("en-US", {
    timeZone: zone,
    hourCycle: "h23",
    year: "numeric",
    month: "numeric",
    day: "numeric",
    hour: "numeric",
    minute: "numeric",
    second: "numeric",
  });
  const wall = Date.UTC(y, m - 1, d, h, mi, s);
  let t = wall;
  for (let i = 0; i < 3; i++) {
    const parts = Object.fromEntries(format.formatToParts(new Date(t)).map((x) => [x.type, Number(x.value)]));
    t += wall - Date.UTC(parts.year!, parts.month! - 1, parts.day!, parts.hour!, parts.minute!, parts.second!);
  }
  return timestampFromDate(new Date(t));
}

const failure = (p: Promise<unknown>) => p.then(() => undefined, (e: unknown) => ConnectError.from(e));

/** The code of a failure and the field it names: its BadRequest violation, or an UNIMPLEMENTED's ErrorInfo "field". */
async function refusal(call: Promise<unknown>): Promise<[Code | undefined, string | undefined]> {
  const err = await failure(call);
  const bad = err?.findDetails(BadRequestSchema)[0]?.fieldViolations;
  if (bad) expect(bad).toHaveLength(1);
  const info = err?.findDetails(ErrorInfoSchema)[0];
  if (err?.code === Code.Unimplemented) expect(info?.reason).toBe("NOT_IMPLEMENTED");
  return [err?.code, bad?.[0]?.field ?? info?.metadata.field];
}

/**
 * The fixture, in creation order: n, title, and what is done to it after
 * its capture (all of an Item's writes come before the next Item's, so
 * updated_at orders them by n). Item 18 comes last with the earliest
 * created_at, as a spool import does.
 */
type Write = (n: number) => Promise<unknown>;

/** Registers the suite; `open` gives a fresh daemon. */
export function filterViewsConformance(open: () => Promise<FilterTarget>): void {
  /** A target with the fixture, its Items' ids by n, and list helpers. */
  async function build() {
    const t = await open();
    const items = t.items;
    const zone = t.timeZone;
    const SUN = localTime(zone, 2026, 10, 25);
    const MON = localTime(zone, 2026, 10, 26);
    const ids = new Map<number, string>();
    const id = (n: number) => ids.get(n)!;
    const update = (fields: MessageInitShape<typeof ItemSchema>, paths: string[]): Write => (n) => updateItem(items, { ...fields, id: id(n) }, paths);
    const to = (s: Status): Write => (n) => setStatus(items, id(n), s);
    const archive: Write = (n) => archiveItem(items, id(n));
    const rows: [number, string, Write[]][] = [
      [1, "Fix flaky login test", [update({ priority: Priority.HIGH, dueDate: date(2026, 10, 24), labels: ["auth", "oncall"] }, ["priority", "due_date", "labels"])]],
      [2, "Renew passport", [to(Status.NEXT), update({ priority: Priority.MEDIUM, dueDate: date(2026, 10, 25) }, ["priority", "due_date"])]],
      [3, "Ship auth migration", [to(Status.IN_PROGRESS), update({ priority: Priority.HIGHEST, localPriorityOverride: Priority.LOW }, ["priority", "local_priority_override"])]],
      [4, "Call dentist", [update({ labels: ["Auth"] }, ["labels"])]],
      [
        5,
        "Café visit",
        [update({ priority: Priority.LOWEST, dueDate: date(2026, 10, 26), description: "plat 530 notes" }, ["priority", "due_date", "description"]), to(Status.SOMEDAY)],
      ],
      [6, "Done thing", [update({ priority: Priority.HIGH, dueDate: date(2026, 10, 1) }, ["priority", "due_date"]), to(Status.DONE)]],
      [7, "Cancelled thing", [to(Status.CANCELLED)]],
      [8, "Deferred tomorrow", [to(Status.NEXT), update({ priority: Priority.HIGH, deferUntil: date(2026, 10, 26) }, ["priority", "defer_until"])]],
      [9, "Deferred today", [to(Status.NEXT), update({ deferUntil: date(2026, 10, 25) }, ["defer_until"])]],
      [
        10,
        "Waiting on vendor",
        [
          to(Status.NEXT),
          update(
            {
              priority: Priority.LOW,
              waiting: { on: { ref: { case: "freeText", value: "the vendor" } }, reason: "", since: date(2026, 10, 20), followUpOn: date(2026, 10, 25) },
            },
            ["priority", "waiting"],
          ),
        ],
      ],
      [
        11,
        "Waiting later",
        [
          to(Status.IN_PROGRESS),
          update({ waiting: { on: { ref: { case: "freeText", value: "Dana" } }, reason: "spec", since: date(2026, 10, 22), followUpOn: date(2026, 10, 30) } }, ["waiting"]),
        ],
      ],
      [12, "Focus item", [to(Status.NEXT), update({ focusWeek: "2026-W43" }, ["focus_week"])]],
      [13, "Next week focus", [to(Status.NEXT), update({ focusWeek: "2026-W44" }, ["focus_week"])]],
      [14, "Archived open", [to(Status.NEXT), update({ dueDate: date(2026, 10, 20) }, ["due_date"]), archive]],
      [15, "Archived done", [to(Status.DONE), archive]],
      [16, "Deleted", [(n) => deleteItem(items, id(n))]],
      [17, "Archived deferred", [to(Status.SOMEDAY), update({ deferUntil: date(2026, 11, 30) }, ["defer_until"]), archive]],
      [19, "Plus label", [update({ labels: ["c++"] }, ["labels"])]],
      [20, "Tiếng Việt NFC".normalize("NFC"), []],
      [21, "Tiếng Việt NFD".normalize("NFD"), []],
      [22, "PLAT-530 follow up", [to(Status.NEXT), update({ storyPoints: 3 }, ["story_points"])]],
      [25, "Ａ fullwidth", [to(Status.SOMEDAY), update({ storyPoints: 0.5 }, ["story_points"])]],
      [26, "\u{1f600} grin", [to(Status.SOMEDAY)]],
      [29, 'say "hi" -minus a*b title:x', []],
      [30, "Year-end focus", [update({ focusWeek: "2026-W53" }, ["focus_week"])]],
      [18, "Spool import", []],
    ];
    for (const [n, title, writes] of rows) {
      // Created a minute apart in n order; 18 the day before.
      const at = timestampFromDate(new Date(n === 18 ? Date.UTC(2026, 8, 30) : Date.UTC(2026, 9, 1, 0, n)));
      const res = await items.captureItem({ requestId: ulid(), text: title, createdVia: CreatedVia.TUI, referenceTime: at });
      ids.set(n, res.item!.id);
      for (const w of writes) await w(n);
    }
    const byId = new Map([...ids].map(([n, i]) => [i, n]));
    const numbers = (list: readonly { id: string }[]) => list.map((i) => byId.get(i.id) ?? -1);

    const views = (await listViews(createClient(ViewService, t.transport))).views;
    const view = (seed: string) => views.find((v) => v.seed === seed)!.id;

    /** One page of a listing, on Sunday unless the request says otherwise. */
    const page = (req: ListInit) => items.listItems({ referenceTime: SUN, ...req });
    /** Every page of a listing, as Item numbers. */
    const list = async (req: ListInit = {}, pageSize = 0): Promise<number[]> => {
      const out: number[] = [];
      let pageToken = "";
      do {
        const res = await page({ ...req, pageSize, pageToken });
        out.push(...numbers(res.items));
        pageToken = res.nextPageToken;
      } while (pageToken !== "");
      return out;
    };
    const where = (...allOf: ClauseInit[]) => list({ filter: { allOf } });
    const filtered = (filter: FilterInit, sort: SortInit[] = []) => list({ filter, sort });
    return { t, items, zone, SUN, MON, id, numbers, views, view, page, list, where, filtered };
  }

  /** What the implicit rules let by, by created_at: 18 first. */
  const VISIBLE = [18, 1, 2, 3, 4, 5, 9, 10, 11, 12, 13, 19, 20, 21, 22, 25, 26, 29, 30];
  const minus = (...ns: number[]) => VISIBLE.filter((n) => !ns.includes(n));
  const BY_SEED: Record<string, number[]> = {
    inbox: [18, 1, 4, 19, 20, 21, 29, 30],
    // Overdue first; the override ranks 3 low, with 10; unset due dates last.
    today: [1, 2, 3, 10, 11, 12],
    next: [2, 3, 10, 9, 11, 12, 13, 22],
    waiting: [10, 11],
    // By updated_at, last written first.
    someday: [26, 25, 5],
    // Done and cancelled before they are archived, and archived and deferred.
    archive: [17, 15, 14, 7, 6],
  };
  const TODAY_MONDAY = [1, 2, 3, 10, 5, 11, 13];

  describe("filters, sorts, paging, views and counts", () => {
    let fx: Awaited<ReturnType<typeof build>>;
    beforeAll(async () => {
      fx = await build();
    }, 60_000);
    afterAll(async () => {
      await fx?.t.close?.();
    });

    it("seeds the six default views, in tab order", () => {
      expect(fx.views.map((v) => [v.position, v.seed, v.name, v.showCount])).toEqual([
        [0, "inbox", "Inbox", true],
        [1, "today", "Today", true],
        [2, "next", "Next", true],
        [3, "waiting", "Waiting", true],
        [4, "someday", "Someday", false],
        [5, "archive", "Archive", false],
      ]);
      const definition = (v: View) => {
        const json = toJson(ViewSchema, v) as Record<string, unknown>;
        for (const k of ["id", "revision", "createdAt", "updatedAt"]) delete json[k];
        return json;
      };
      expect(fx.views.map(definition)).toEqual(defaultViews().map((v) => definition(create(ViewSchema, v))));
      for (const v of fx.views) {
        expect(v.id).toMatch(/^[0-9A-HJKMNP-TV-Z]{26}$/);
        expect(v.revision).toBeGreaterThan(0n);
        expect(v.createdAt).toBeDefined();
      }
    });

    it("streams view changes to the subscribers of their topic only", async () => {
      const watch = createClient(WatchService, fx.t.transport);
      /** The replay from revision 0 up to CaughtUp, as each change's topic and id. */
      const replay = async (topics: Topic[]) => {
        const ac = new AbortController();
        const changes: [Topic, string][] = [];
        try {
          for await (const res of watch.subscribe({ sinceRevision: 0n, topics }, { signal: ac.signal })) {
            if (res.message.case === "caughtUp") break;
            if (res.message.case === "change") changes.push([res.message.value.topic, res.message.value.id]);
          }
        } finally {
          ac.abort();
        }
        return changes;
      };
      expect(await replay([Topic.VIEW])).toEqual(fx.views.map((v) => [Topic.VIEW, v.id]));
      const items = await replay([Topic.ITEM]);
      expect(items.length).toBeGreaterThan(0);
      expect(items.filter(([topic]) => topic !== Topic.ITEM)).toEqual([]);
      const all = await replay([]);
      expect(all.filter(([topic]) => topic === Topic.VIEW)).toHaveLength(fx.views.length);
      expect(await refusal(replay([Topic.UNSPECIFIED]))).toEqual([Code.InvalidArgument, "topics"]);
    });

    it("lists each default view's members in its order", async () => {
      for (const [seed, want] of Object.entries(BY_SEED)) {
        expect(await fx.list({ viewId: fx.view(seed) }), seed).toEqual(want);
      }
      expect(await fx.list({ viewId: fx.view("today"), referenceTime: fx.MON })).toEqual(TODAY_MONDAY);
      // The deferred Item shows up the day it is deferred to.
      expect(await fx.list({ viewId: fx.view("next"), referenceTime: fx.MON })).toEqual([8, 2, 3, 10, 9, 11, 12, 13, 22]);
    });

    it("takes today in the daemon's zone, around midnight and a DST change", async () => {
      const { zone } = fx;
      const today = { viewId: fx.view("today") };
      expect(await fx.list({ ...today, referenceTime: localTime(zone, 2026, 10, 25, 23, 59, 59) })).toEqual(BY_SEED.today);
      expect(await fx.list({ ...today, referenceTime: localTime(zone, 2026, 10, 26, 0, 0, 0) })).toEqual(TODAY_MONDAY);
      const thisWeek = (y: number, m: number, d: number, h = 12, mi = 0) =>
        fx.list({ filter: { allOf: [p(F.FOCUS_WEEK, O.EQ, rel(Anchor.CURRENT_WEEK))] }, referenceTime: localTime(zone, y, m, d, h, mi) });
      expect(await thisWeek(2026, 10, 25)).toEqual([12]);
      expect(await thisWeek(2026, 10, 26)).toEqual([13]);
      // The ISO week-year: 2027-01-01 is in 2026-W53.
      expect(await thisWeek(2027, 1, 1)).toEqual([30]);
      expect(await thisWeek(2027, 1, 4)).toEqual([]);
      // Late on the Sunday the clocks go back is still that Sunday's week.
      expect(await thisWeek(2026, 11, 1, 23, 30)).toEqual([13]);
    });

    it("applies the implicit rules, which only the include_* flags lift", async () => {
      expect(await fx.list()).toEqual(VISIBLE);
      expect(await fx.where(p(F.STATUS, O.IN, status(Status.DONE)))).toEqual([]);
      expect(await fx.filtered({ allOf: [p(F.STATUS, O.IN, status(Status.DONE))], includeDone: true })).toEqual([6]);
      expect(await fx.filtered({ allOf: [p(F.STATUS, O.IN, status(Status.DONE))], includeDone: true, includeArchived: true })).toEqual([6, 15]);
      expect(await fx.where(p(F.DELETED_AT, O.IS_SET))).toEqual([]);
      expect(await fx.filtered({ allOf: [p(F.DELETED_AT, O.IS_SET)], includeDeleted: true })).toEqual([16]);
      const deferred = p(F.TITLE, O.IN, str("Deferred today"), str("Deferred tomorrow"));
      expect(await fx.where(deferred)).toEqual([9]);
      expect(await fx.list({ filter: { allOf: [deferred] }, referenceTime: fx.MON })).toEqual([8, 9]);
      expect(await fx.filtered({ allOf: [p(F.ARCHIVED, O.IS_SET)], includeArchived: true, includeDone: true, includeDeferred: true })).toEqual([14, 15, 17]);
    });

    it("matches unset values with NE only, and nested ones under an absent message", async () => {
      expect(await fx.where(p(F.FOCUS_WEEK, O.NE, rel(Anchor.CURRENT_WEEK)))).toEqual(minus(12));
      expect(await fx.where(p(F.WAITING_REASON, O.EQ, str("")))).toEqual([10]);
      expect(await fx.where(p(F.WAITING_REASON, O.NE, str("spec")))).toEqual(minus(11));
      expect(await fx.where(p(F.WAITING_ON, O.EQ, free("the vendor")))).toEqual([10]);
      expect(await fx.where(p(F.WAITING_ON, O.EQ, free("The Vendor")))).toEqual([]);
      expect(await fx.where(p(F.WAITING_ON, O.NE, free("Dana")))).toEqual(minus(11));
      expect(await fx.where(p(F.DUE_DATE, O.IS_UNSET))).toEqual(minus(1, 2, 5));
      expect(await fx.where(p(F.DUE_DATE, O.LT, value({ case: "dateValue", value: date(2026, 10, 26) })))).toEqual([1, 2]);
      expect(await fx.where(p(F.STORY_POINTS, O.GE, value({ case: "numberValue", value: 1 })))).toEqual([22]);
      expect(await fx.where(p(F.STORY_POINTS, O.LT, value({ case: "numberValue", value: 1 })))).toEqual([25]);
      expect(await fx.where(p(F.STORY_POINTS, O.NE, value({ case: "numberValue", value: 3 })))).toEqual(minus(22));
      expect(await fx.filtered({ allOf: [p(F.IS_OVERDUE, O.EQ, value({ case: "boolValue", value: true }))], includeDone: true })).toEqual([1]);
      expect(
        await fx.filtered({ allOf: [p(F.IS_OVERDUE, O.EQ, value({ case: "boolValue", value: true }))], includeDone: true, includeArchived: true }),
      ).toEqual([1, 14]);
      expect(await fx.where(p(F.ID, O.IN, value({ case: "itemId", value: fx.id(1).toLowerCase() })))).toEqual([1]);
    });

    it("treats sets as never unset, with exact, case-sensitive membership", async () => {
      const label = (...ls: string[]) => ls.map(str);
      expect(await fx.where(p(F.LABELS, O.CONTAINS_ANY, ...label("auth")))).toEqual([1]);
      expect(await fx.where(p(F.LABELS, O.CONTAINS_ANY, ...label("Auth", "c++")))).toEqual([4, 19]);
      expect(await fx.where(p(F.LABELS, O.CONTAINS_ALL, ...label("auth", "auth", "oncall")))).toEqual([1]);
      expect(await fx.where(p(F.LABELS, O.CONTAINS_ALL, ...label("auth", "nope")))).toEqual([]);
      expect(await fx.where(p(F.LABELS, O.IS_SET))).toEqual([1, 4, 19]);
      expect(await fx.where(p(F.LABELS, O.IS_UNSET))).toEqual(minus(1, 4, 19));
    });

    it("orders priorities by rank with NONE outside the order and last both ways", async () => {
      const next = { allOf: [p(F.STATUS, O.IN, status(Status.NEXT))] };
      expect(await fx.filtered(next, [asc(F.EFFECTIVE_PRIORITY)])).toEqual([10, 2, 9, 12, 13, 22]);
      expect(await fx.filtered(next, [desc(F.EFFECTIVE_PRIORITY)])).toEqual([2, 10, 9, 12, 13, 22]);
      expect(await fx.where(p(F.EFFECTIVE_PRIORITY, O.GE, prio(Priority.HIGH)))).toEqual([1]);
      expect(await fx.where(p(F.PRIORITY, O.GE, prio(Priority.HIGH)))).toEqual([1, 3]);
      expect(await fx.where(p(F.EFFECTIVE_PRIORITY, O.LT, prio(Priority.MEDIUM)))).toEqual([3, 5, 10]);
      expect(await fx.where(p(F.PRIORITY, O.EQ, prio(Priority.NONE)))).toEqual([18, 4, 9, 11, 12, 13, 19, 20, 21, 22, 25, 26, 29, 30]);
      expect(await fx.where(p(F.LOCAL_PRIORITY_OVERRIDE, O.IS_SET))).toEqual([3]);
    });

    it("sorts strings by code point, unset values last both ways, and by the tie-break", async () => {
      const someday = { allOf: [p(F.STATUS, O.IN, status(Status.SOMEDAY))] };
      // "Café" < "Ａ" (U+FF21) < "😀" (U+1F600); UTF-16 order puts the emoji first.
      expect(await fx.filtered(someday, [asc(F.TITLE)])).toEqual([5, 25, 26]);
      expect(await fx.filtered(someday, [desc(F.TITLE)])).toEqual([26, 25, 5]);
      const open = { allOf: [p(F.STATUS, O.IN, status(Status.NEXT), status(Status.IN_PROGRESS))] };
      expect(await fx.filtered(open, [asc(F.DUE_DATE)])).toEqual([2, 3, 9, 10, 11, 12, 13, 22]);
      expect(await fx.filtered(open, [desc(F.DUE_DATE)])).toEqual([2, 3, 9, 10, 11, 12, 13, 22]);
      expect(await fx.filtered({ allOf: [p(F.WAITING, O.IS_SET)] }, [desc(F.WAITING_SINCE)])).toEqual([11, 10]);
      expect(await fx.filtered({}, [desc(F.CREATED_AT)])).toEqual([...VISIBLE].reverse());
      expect(await fx.filtered({ allOf: [p(F.FOCUS_WEEK, O.IS_SET)] }, [desc(F.FOCUS_WEEK)])).toEqual([30, 13, 12]);
    });

    it("resolves relative dates and nests groups", async () => {
      const week = [p(F.DUE_DATE, O.GE, rel(Anchor.START_OF_WEEK)), p(F.DUE_DATE, O.LE, rel(Anchor.END_OF_WEEK))];
      expect(await fx.where(...week)).toEqual([1, 2]);
      expect(await fx.list({ filter: { allOf: week }, referenceTime: fx.MON })).toEqual([5]);
      expect(await fx.where(p(F.DUE_DATE, O.EQ, rel(Anchor.TODAY, -1)))).toEqual([1]);
      expect(await fx.where(p(F.FOCUS_WEEK, O.LT, rel(Anchor.CURRENT_WEEK)))).toEqual([]);
      expect(await fx.where(p(F.FOCUS_WEEK, O.GT, rel(Anchor.CURRENT_WEEK)))).toEqual([13, 30]);
      const nested = group([], [p(F.STATUS, O.IN, status(Status.INBOX)), group([p(F.STATUS, O.IN, status(Status.NEXT)), p(F.EFFECTIVE_PRIORITY, O.GE, prio(Priority.MEDIUM))])]);
      expect(await fx.where(nested)).toEqual([18, 1, 2, 4, 19, 20, 21, 29, 30]);
      expect(await fx.where(group([]))).toEqual(VISIBLE);
      expect(await fx.filtered({ anyOf: [p(F.STATUS, O.IN, status(Status.IN_PROGRESS)), p(F.LABELS, O.CONTAINS_ANY, str("auth"))] })).toEqual([1, 3, 11]);
    });

    it("searches text with MATCHES: words in order, the last a prefix, ignoring case and accents", async () => {
      const m = (field: ItemField, t: string) => fx.where(p(field, O.MATCHES, text(t)));
      expect(await m(F.TITLE, "plat-530")).toEqual([22]);
      expect(await m(F.DESCRIPTION, "plat-530")).toEqual([5]);
      expect(await fx.filtered({ anyOf: [p(F.TITLE, O.MATCHES, text("530 plat")), p(F.DESCRIPTION, O.MATCHES, text("530 plat"))] })).toEqual([5, 22]);
      expect(await m(F.TITLE, "pla 53")).toEqual([22]);
      expect(await m(F.TITLE, "pla-53")).toEqual([]);
      expect(await m(F.TITLE, "CAFÉ")).toEqual([5]);
      expect(await m(F.TITLE, "cafe")).toEqual([5]);
      expect(await m(F.TITLE, "tieng")).toEqual([20, 21]);
      expect(await m(F.TITLE, "VIỆT")).toEqual([20, 21]);
      expect(await m(F.TITLE, "c++")).toEqual([4, 5]);
      expect(await m(F.LABELS, "c++")).toEqual([19]);
      expect(await m(F.LABELS, "auth")).toEqual([1, 4]);
      expect(await m(F.TITLE, "auth")).toEqual([3]);
      expect(await m(F.LABELS, "auth,oncall")).toEqual([1]);
      for (const t of ['"hi"', "title:x", "a*b", "-minus", "SAY hi"]) expect(await m(F.TITLE, t), t).toEqual([29]);
      expect(await m(F.TITLE, "-")).toEqual([]);
      expect(await m(F.TITLE, "fix -")).toEqual([1]);
      expect(await fx.filtered({ anyOf: [p(F.LABELS, O.MATCHES, text("oncall")), p(F.STATUS, O.IN, status(Status.IN_PROGRESS))] })).toEqual([1, 3, 11]);
      // Deleted and archived Items are indexed too; the implicit rules still apply.
      expect(await m(F.TITLE, "archived")).toEqual([]);
      expect(await fx.filtered({ allOf: [p(F.TITLE, O.MATCHES, text("archived"))], includeArchived: true, includeDeferred: true, includeDone: true })).toEqual([
        14, 15, 17,
      ]);
    });

    it("narrows a view with view_id: its filter AND the request's, flags OR-ed, the request's sort", async () => {
      expect(await fx.list({ viewId: fx.view("today"), filter: { allOf: [p(F.LABELS, O.CONTAINS_ANY, str("auth"))] } })).toEqual([1]);
      expect(await fx.list({ viewId: fx.view("today"), filter: { allOf: [p(F.TITLE, O.MATCHES, text("re"))] } })).toEqual([2]);
      expect(await fx.list({ viewId: fx.view("archive"), filter: { allOf: [p(F.STATUS, O.IN, status(Status.DONE))] } })).toEqual([15, 6]);
      expect(await fx.list({ viewId: fx.view("inbox"), sort: [desc(F.CREATED_AT)] })).toEqual([...BY_SEED.inbox!].reverse());
      expect(await fx.list({ viewId: fx.view("next"), filter: { includeDeferred: true } })).toEqual([8, 2, 3, 10, 9, 11, 12, 13, 22]);
      expect(await fx.list({ viewId: fx.view("waiting").toLowerCase() })).toEqual(BY_SEED.waiting);

      const missing = ulid();
      const err = await failure(fx.page({ viewId: missing }));
      expect(err?.code).toBe(Code.NotFound);
      const info = err?.findDetails(ErrorInfoSchema)[0];
      expect([info?.reason, info?.metadata]).toEqual(["VIEW_NOT_FOUND", { view_id: missing }]);
      expect(await refusal(fx.page({ viewId: "nope" }))).toEqual([Code.InvalidArgument, "view_id"]);
    });

    it("pages with keyset tokens: any page size gives the unpaged list", async () => {
      const listings: [string, ListInit][] = [
        ["everything", {}],
        ["next by priority", { filter: { allOf: [p(F.STATUS, O.IN, status(Status.NEXT))] }, sort: [desc(F.EFFECTIVE_PRIORITY)] }],
        ["someday by title", { filter: { allOf: [p(F.STATUS, O.IN, status(Status.SOMEDAY))] }, sort: [asc(F.TITLE)] }],
        ["due, unset last", { sort: [desc(F.DUE_DATE), asc(F.FOCUS_WEEK)] }],
        ...fx.views.map((v): [string, ListInit] => [v.seed!, { viewId: v.id }]),
      ];
      for (const [name, req] of listings) {
        const whole = await fx.list(req, 1000);
        expect(whole.length, name).toBeGreaterThan(0);
        for (const size of [1, 2]) expect(await fx.list(req, size), `${name} by ${size}`).toEqual(whole);
      }
      // Page size 0 is the daemon's default, which holds the whole fixture.
      const first = await fx.page({ pageSize: 0 });
      expect([fx.numbers(first.items), first.nextPageToken]).toEqual([VISIBLE, ""]);
    });

    it("binds a page token to the effective filter and sort, not the page size, and keeps its today", async () => {
      const first = await fx.page({ pageSize: 2 });
      expect(fx.numbers(first.items)).toEqual([18, 1]);
      const token = first.nextPageToken;
      expect(token).not.toBe("");
      const rest = await fx.page({ pageSize: 100, pageToken: token });
      expect(fx.numbers(rest.items)).toEqual(VISIBLE.slice(2));
      expect(rest.nextPageToken).toBe("");
      const other = { allOf: [p(F.STATUS, O.IN, status(Status.INBOX))] };
      expect(await refusal(fx.page({ filter: other, pageSize: 2, pageToken: token }))).toEqual([Code.InvalidArgument, "page_token"]);
      expect(await refusal(fx.page({ sort: [desc(F.CREATED_AT)], pageSize: 2, pageToken: token }))).toEqual([Code.InvalidArgument, "page_token"]);
      expect(await refusal(fx.page({ pageToken: "garbage" }))).toEqual([Code.InvalidArgument, "page_token"]);
      // A view's token is bound to that view's effective filter.
      const today = await fx.page({ viewId: fx.view("today"), pageSize: 2 });
      expect(await refusal(fx.page({ viewId: fx.view("next"), pageSize: 2, pageToken: today.nextPageToken }))).toEqual([Code.InvalidArgument, "page_token"]);
      // Today is the first page's: Monday's reference time on page 2 still
      // finishes Sunday's list.
      expect(fx.numbers(today.items)).toEqual([1, 2]);
      const monday = await fx.page({ viewId: fx.view("today"), pageSize: 100, pageToken: today.nextPageToken, referenceTime: fx.MON });
      expect(fx.numbers(monday.items)).toEqual(BY_SEED.today!.slice(2));
      // Its Items are as of that today too.
      expect(monday.items.map((i) => i.isOverdue)).toEqual([false, false, false, false]);
    });

    it("counts with the same rules as the listings", async () => {
      for (const [at, today] of [
        [fx.SUN, date(2026, 10, 25)],
        [fx.MON, date(2026, 10, 26)],
      ] as const) {
        const res = await getStatusCounts(fx.items, { includeViews: true, referenceTime: at });
        expect(res.today).toMatchObject(today);
        const inList = async (...allOf: ClauseInit[]) => (await fx.list({ filter: { allOf }, referenceTime: at })).length;
        expect(res.counts).toMatchObject({
          inbox: await inList(p(F.STATUS, O.IN, status(Status.INBOX))),
          due: await inList(p(F.DUE_DATE, O.LE, rel(Anchor.TODAY))),
          overdue: await inList(p(F.DUE_DATE, O.LT, rel(Anchor.TODAY))),
        });
        const counted = fx.views.filter((v) => v.showCount);
        expect(res.views.map((v) => v.viewId)).toEqual(counted.map((v) => v.id));
        for (const v of res.views) {
          expect(v.count, v.viewId).toBe((await fx.list({ viewId: v.viewId, referenceTime: at })).length);
        }
      }
      const sunday = await getStatusCounts(fx.items, { referenceTime: fx.SUN });
      expect(sunday.counts).toMatchObject({ inbox: 8, due: 2, overdue: 1 });
      expect(sunday.views).toEqual([]);
      expect((await getStatusCounts(fx.items, { referenceTime: fx.MON })).counts).toMatchObject({ inbox: 8, due: 3, overdue: 2 });
      expect(sunday.revision).toBe((await fx.page({ pageSize: 1 })).revision);
    });

    it("refuses invalid filters and sorts on their first path in document order", async () => {
      const at = "filter.all_of[0].predicate";
      const ok = p(F.STATUS, O.IN, status(Status.INBOX));
      const deep = [0, 1, 2, 3, 4].reduce<ClauseInit>((c) => group([c]), ok);
      const cases: [string, ListInit, string][] = [
        ["NONE in an ordering", { filter: { allOf: [p(F.PRIORITY, O.LT, prio(Priority.NONE))] } }, `${at}.operands[0]`],
        ["EQ on a set", { filter: { allOf: [p(F.LABELS, O.EQ, str("auth"))] } }, `${at}.op`],
        ["IS_SET on a target that is never unset", { filter: { allOf: [p(F.STATUS, O.IS_SET)] } }, `${at}.op`],
        ["CURRENT_WEEK on a date", { filter: { allOf: [p(F.DUE_DATE, O.EQ, rel(Anchor.CURRENT_WEEK))] } }, `${at}.operands[0]`],
        ["TODAY on focus_week", { filter: { allOf: [p(F.FOCUS_WEEK, O.EQ, rel(Anchor.TODAY))] } }, `${at}.operands[0]`],
        ["an offset on CURRENT_WEEK", { filter: { allOf: [p(F.FOCUS_WEEK, O.EQ, rel(Anchor.CURRENT_WEEK, 1))] } }, `${at}.operands[0].relative_date.offset_days`],
        ["a date out of range", { filter: { allOf: [p(F.DUE_DATE, O.EQ, rel(Anchor.TODAY, 2147483647))] } }, `${at}.operands[0].relative_date.offset_days`],
        ["an unspecified anchor", { filter: { allOf: [p(F.DUE_DATE, O.EQ, rel(Anchor.UNSPECIFIED))] } }, `${at}.operands[0].relative_date.anchor`],
        ["a clause without a kind", { filter: { anyOf: [ok, {}] } }, "filter.any_of[1]"],
        [
          "all_of before any_of",
          { filter: { allOf: [ok, p(F.TITLE, O.LT, str("x"))], anyOf: [{ kind: { case: "predicate", value: { target: 999 as ItemField, op: O.EQ } } }] } },
          "filter.all_of[1].predicate.op",
        ],
        ["inside a group", { filter: { allOf: [group([], [ok, p(F.STATUS, O.LT, status(Status.DONE))])] } }, "filter.all_of[0].group.any_of[1].predicate.op"],
        ["a NaN", { filter: { allOf: [p(F.STORY_POINTS, O.EQ, value({ case: "numberValue", value: Number.NaN }))] } }, `${at}.operands[0]`],
        ["no such date", { filter: { allOf: [p(F.DUE_DATE, O.EQ, value({ case: "dateValue", value: date(2026, 2, 30) }))] } }, `${at}.operands[0]`],
        [
          "an instant past 2262",
          { filter: { allOf: [p(F.CREATED_AT, O.GE, value({ case: "timestampValue", value: timestampFromDate(new Date(Date.UTC(2300, 0, 1))) }))] } },
          `${at}.operands[0]`,
        ],
        ["an id that is no ULID", { filter: { allOf: [p(F.ID, O.EQ, value({ case: "itemId", value: "nope" }))] } }, `${at}.operands[0]`],
        ["free text on assignee", { filter: { allOf: [p(F.ASSIGNEE, O.EQ, free("dana"))] } }, `${at}.operands[0]`],
        ["cleared", { filter: { allOf: [p(F.DUE_DATE, O.EQ, value({ case: "cleared", value: {} }))] } }, `${at}.operands[0]`],
        ["STATUS_UNSPECIFIED", { filter: { allOf: [p(F.STATUS, O.EQ, status(Status.UNSPECIFIED))] } }, `${at}.operands[0]`],
        ["the wrong value kind", { filter: { allOf: [p(F.STATUS, O.EQ, prio(Priority.HIGH))] } }, `${at}.operands[0]`],
        ["two operands for EQ", { filter: { allOf: [p(F.STATUS, O.EQ, status(Status.INBOX), status(Status.NEXT))] } }, `${at}.operands`],
        ["blank MATCHES text", { filter: { allOf: [p(F.TITLE, O.MATCHES, text("   "))] } }, `${at}.operands[0]`],
        ["MATCHES on a target it does not search", { filter: { allOf: [p(F.PRIVATE_NOTES, O.MATCHES, text("x"))] } }, `${at}.target`],
        ["MATCHES with a value", { filter: { allOf: [p(F.TITLE, O.MATCHES, str("x"))] } }, `${at}.operands[0]`],
        ["text without MATCHES", { filter: { allOf: [p(F.TITLE, O.EQ, text("x"))] } }, `${at}.operands[0]`],
        ["groups 5 deep", { filter: { allOf: [deep] } }, `filter${".all_of[0].group".repeat(5)}`],
        ["129 predicates", { filter: { allOf: Array.from({ length: 129 }, () => ok) } }, "filter"],
        ["a set as a sort key", { sort: [asc(F.LABELS)] }, "sort[0].key"],
        ["a repeated sort key", { sort: [asc(F.DUE_DATE), desc(F.DUE_DATE)] }, "sort[1].key"],
        [
          "nine sort keys",
          { sort: [F.STATUS, F.PRIORITY, F.DUE_DATE, F.DEFER_UNTIL, F.CREATED_AT, F.UPDATED_AT, F.TITLE, F.ID, F.REVISION].map(asc) },
          "sort",
        ],
        ["a negative page size", { pageSize: -1 }, "page_size"],
      ];
      for (const [name, req, field] of cases) {
        expect(await refusal(fx.page(req)), name).toEqual([Code.InvalidArgument, field]);
      }
    });

    it("answers valid but unbuilt filters with UNIMPLEMENTED, unless something is invalid", async () => {
      const at = "filter.all_of[0].predicate";
      expect(await refusal(fx.page({ filter: { allOf: [p(F.REMOTE_EXTRAS, O.IS_SET)] } }))).toEqual([Code.Unimplemented, `${at}.target`]);
      expect(await refusal(fx.page({ filter: { allOf: [p(F.CREATED_AT, O.LE, rel(Anchor.TODAY))] } }))).toEqual([Code.Unimplemented, `${at}.operands[0]`]);
      expect(await refusal(fx.page({ filter: { allOf: [p(F.PENDING_FIELDS, O.CONTAINS_ANY, str("x"))] } }))).toEqual([Code.Unimplemented, `${at}.op`]);
      expect(await refusal(fx.page({ filter: { allOf: [p(F.REMOTE_EXTRAS, O.IS_SET), p(F.TITLE, O.LT, str("x"))] } }))).toEqual([
        Code.InvalidArgument,
        "filter.all_of[1].predicate.op",
      ]);
      expect(await refusal(fx.page({ filter: { allOf: [p(F.REMOTE_EXTRAS, O.IS_SET)] }, sort: [asc(F.LABELS)] }))).toEqual([Code.InvalidArgument, "sort[0].key"]);
    });

    it("parses dates with the capture grammar on the reference day, in the daemon's zone", async () => {
      const on = async (t: string, at: Timestamp = fx.SUN) => {
        const res = await parseDate(fx.items, t, { referenceTime: at });
        return res.date ? [res.date.year, res.date.month, res.date.day] : res.warning;
      };
      const rows: [string, [number, number, number]][] = [
        ["today", [2026, 10, 25]],
        ["tom", [2026, 10, 26]],
        ["fri", [2026, 10, 30]],
        ["^fri", [2026, 10, 30]],
        ["  FRIDAY ", [2026, 10, 30]],
        ["sun", [2026, 11, 1]],
        ["+3d", [2026, 10, 28]],
        ["+2w", [2026, 11, 8]],
        ["+1m", [2026, 11, 25]],
        ["eow", [2026, 10, 30]],
        ["eom", [2026, 10, 31]],
        ["nov2", [2026, 11, 2]],
        ["oct25", [2026, 10, 25]],
        ["oct24", [2027, 10, 24]],
        ["feb29", [2028, 2, 29]],
        ["2026-11-02", [2026, 11, 2]],
      ];
      for (const [t, want] of rows) expect(await on(t), t).toEqual(want);
      expect(await on("fridays")).toBe("fridays: not a date");
      expect(await on("feb30")).toBe("feb30: february has no day 30");
      expect(await on("^nope")).toBe("^nope: not a date");
      expect(await on("")).toBe("empty date");
      expect(await on("tom", localTime(fx.zone, 2026, 10, 25, 23, 30))).toEqual([2026, 10, 26]);
      expect(await on("tom", localTime(fx.zone, 2026, 10, 26, 0, 30))).toEqual([2026, 10, 27]);
    });
  });

  describe("paging over live data", () => {
    let t: FilterTarget | undefined;
    afterAll(async () => {
      await t?.close?.();
    });

    it("continues after the cursor when rows change between pages, and keeps nanoseconds apart", async () => {
      t = await open();
      const items = t.items;
      const SUN = localTime(t.timeZone, 2026, 10, 25);
      const capture = async (title: string, at: Timestamp, priority?: Priority) => {
        const { item } = await items.captureItem({ requestId: ulid(), text: title, createdVia: CreatedVia.TUI, referenceTime: at });
        await setStatus(items, item!.id, Status.NEXT);
        if (priority !== undefined) await updateItem(items, { id: item!.id, priority }, ["priority"]);
        return item!.id;
      };
      const minute = (n: number) => timestampFromDate(new Date(Date.UTC(2026, 9, 1, 0, n)));
      const [a, b, c, d, e] = [
        await capture("a", minute(1), Priority.HIGH),
        await capture("b", minute(2), Priority.MEDIUM),
        await capture("c", minute(3), Priority.LOW),
        await capture("d", minute(4)),
        await capture("e", minute(5)),
      ];
      const req = { filter: { allOf: [p(F.STATUS, O.IN, status(Status.NEXT))] }, sort: [desc(F.EFFECTIVE_PRIORITY)], referenceTime: SUN, pageSize: 2 };
      const ids = (res: { items: { id: string }[] }) => res.items.map((i) => i.id);

      // A row completed before the cursor does not shift the next page.
      let first = await items.listItems(req);
      expect(ids(first)).toEqual([a, b]);
      await setStatus(items, a, Status.DONE);
      expect(ids(await items.listItems({ ...req, pageToken: first.nextPageToken }))).toEqual([c, d]);

      // A row whose key moves past the cursor comes again.
      first = await items.listItems(req);
      expect(ids(first)).toEqual([b, c]);
      await updateItem(items, { id: b, priority: Priority.NONE }, ["priority"]);
      expect(ids(await items.listItems({ ...req, pageToken: first.nextPageToken }))).toEqual([b, d]);
      expect(ids(await items.listItems({ ...req, pageSize: 10 }))).toEqual([c, b, d, e]);

      // Created 1 ns apart: the later created_at sorts after, whatever the ids.
      const late = (await items.captureItem({
        requestId: ulid(),
        text: "late",
        createdVia: CreatedVia.TUI,
        referenceTime: { seconds: 1_790_000_000n, nanos: 1 },
      })).item!.id;
      const early = (await items.captureItem({
        requestId: ulid(),
        text: "early",
        createdVia: CreatedVia.TUI,
        referenceTime: { seconds: 1_790_000_000n, nanos: 0 },
      })).item!.id;
      const pair = { filter: { allOf: [p(F.TITLE, O.IN, str("late"), str("early"))] }, referenceTime: SUN };
      expect(ids(await items.listItems(pair))).toEqual([early, late]);
      expect(ids(await items.listItems({ ...pair, sort: [desc(F.CREATED_AT)] }))).toEqual([late, early]);
      expect(ids(await items.listItems({ ...pair, sort: [desc(F.CREATED_AT)], pageSize: 1 }))).toEqual([late]);
    });
  });
}
