// The capture syntax (design 2.3) through ParseCapture and CaptureItem, and
// the waiting writes of the TUI's w prompt, as a suite any daemon runs:
// test/capture-conformance.test.ts runs it against FakeDaemon (whose parser
// is the port in src/capture.ts), uds.integration.test.ts against the real
// daemon ($TODO_BIN), spawned in the same zone. Every parse passes
// reference_time, so "today" is fixed; the zone is far from UTC, so a day
// taken in the wrong zone shows.
import { type Timestamp, timestampDate } from "@bufbuild/protobuf/wkt";
import { Code, ConnectError } from "@connectrpc/connect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { dayIn, dayOf, formatDay } from "../../src/dates.js";
import { BadRequestSchema } from "../../src/gen/google/rpc/error_details_pb.js";
import { CreatedVia, type Item, Priority, Status } from "../../src/gen/todo/v1/item_pb.js";
import { type CaptureDraft, CaptureTokenKind as K } from "../../src/gen/todo/v1/item_service_pb.js";
import { MAX_CAPTURE_BYTES } from "../../src/limits.js";
import { type ItemsClient, batchMutate, updateItem } from "../../src/mutations.js";
import { parseCapture } from "../../src/queries.js";
import { ulid } from "../../src/ulid.js";
import { localTime } from "./filter-views.js";

/** The zone both targets run the suite in: the real daemon is spawned with TZ set to it. */
export const CAPTURE_ZONE = "Pacific/Auckland";

/** A daemon in a known zone, for the whole suite. */
export interface CaptureTarget {
  items: ItemsClient;
  /** The daemon's zone (an IANA name), which its today is taken in. */
  timeZone: string;
  close?: () => void | Promise<void>;
}

/** A token as [start, end, kind, applied, warning]. */
type TokenRow = [number, number, K, boolean, string];

const failure = (p: Promise<unknown>) => p.then(() => undefined, (e: unknown) => ConnectError.from(e));

/** The [field, description] of an INVALID_ARGUMENT failure's single BadRequest violation. */
async function rejected(call: Promise<unknown>): Promise<[string | undefined, string | undefined]> {
  const err = await failure(call);
  expect(err?.code).toBe(Code.InvalidArgument);
  const [bad] = err?.findDetails(BadRequestSchema) ?? [];
  expect(bad?.fieldViolations).toHaveLength(1);
  return [bad?.fieldViolations[0]?.field, bad?.fieldViolations[0]?.description];
}

const iso = (d: { year: number; month: number; day: number } | undefined) => (d === undefined ? undefined : formatDay(dayOf(d)));

const freeText = (value: string) => ({ ref: { case: "freeText" as const, value } });

/** A draft in a form to compare whole. */
function shown(d: CaptureDraft | undefined) {
  if (!d) return undefined;
  return {
    title: d.title,
    priority: d.priority,
    due: iso(d.dueDate),
    labels: d.labels,
    waiting: d.waitingOn?.ref.case === "freeText" ? d.waitingOn.ref.value : d.waitingOn?.ref.case,
    status: d.status,
    tokens: d.tokens.map((t): TokenRow => [t.start, t.end, t.kind, t.applied, t.warning]),
    warnings: d.warnings,
  };
}

/** Registers the suite; `open` gives a daemon for all of it. */
export function captureConformance(open: () => Promise<CaptureTarget>): void {
  describe("capture syntax and waiting", () => {
    let t: CaptureTarget;
    /** Local 08:00 on a day of 2026 (or `year`) in the target's zone: the day before in UTC. */
    let at: (month: number, day: number, year?: number, hour?: number, minute?: number) => Timestamp;
    /** Thursday 2026-10-22. */
    let THU: Timestamp;
    beforeAll(async () => {
      t = await open();
      at = (month, day, year = 2026, hour = 8, minute = 0) => localTime(t.timeZone, year, month, day, hour, minute);
      THU = at(10, 22);
    });
    afterAll(async () => {
      await t?.close?.();
    });

    const parse = async (text: string, referenceTime: Timestamp = THU) => shown((await parseCapture(t.items, text, { referenceTime })).draft);
    const capture = (text: string, extra: { referenceTime?: Timestamp; requestId?: string; spoolId?: string; createdVia?: CreatedVia } = {}) =>
      t.items.captureItem({ requestId: ulid(), text, createdVia: CreatedVia.TUI, referenceTime: THU, ...extra });

    it("parses every token kind into the draft, with code-point offsets", async () => {
      expect(await parse("call dentist ^fri +2 #home ~dana >next")).toEqual({
        title: "call dentist",
        priority: Priority.HIGH,
        due: "2026-10-23",
        labels: ["home"],
        waiting: "dana",
        status: Status.NEXT,
        tokens: [
          [13, 17, K.DUE_DATE, true, ""],
          [18, 20, K.PRIORITY, true, ""],
          [21, 26, K.LABEL, true, ""],
          [27, 32, K.WAITING, true, ""],
          [33, 38, K.STATUS, true, ""],
        ],
        warnings: [],
      });
      const priorities = [Priority.HIGHEST, Priority.HIGH, Priority.MEDIUM, Priority.LOW, Priority.LOWEST];
      for (const [i, p] of priorities.entries()) expect((await parse(`x +${i + 1}`))?.priority).toBe(p);
      expect(await parse("x >SomeDay")).toMatchObject({ title: "x", status: Status.SOMEDAY });
      expect(await parse("x ##a#b #A")).toMatchObject({ labels: ["#a#b", "A"] });
      expect(await parse("call dentist")).toEqual({
        title: "call dentist",
        priority: Priority.NONE,
        due: undefined,
        labels: [],
        waiting: undefined,
        status: Status.INBOX,
        tokens: [],
        warnings: [],
      });
    });

    it("keeps tokens that do not parse in the title, with the daemon's warnings", async () => {
      const rows: [string, string, string][] = [
        ["x +0", "x +0", "+0: priority must be +1 to +5"],
        ["x +6", "x +6", "+6: priority must be +1 to +5"],
        ["x ++1", "x ++1", "++1: priority must be +1 to +5"],
        ["x +1x", "x +1x", "+1x: priority must be +1 to +5"],
        ["x ^someday", "x ^someday", "^someday: not a date"],
        ["x ^feb30", "x ^feb30", "^feb30: february has no day 30"],
        ["x ^nov", "x ^nov", "^nov: a month needs a day, e.g. nov2"],
        ["x ^+3x", "x ^+3x", "^+3x: expected +N followed by d, w or m"],
        ["x ^+99999999999d", "x ^+99999999999d", "^+99999999999d: out of range"],
        ["x ^^fri", "x ^^fri", "^^fri: not a date"],
        ["x ^0000-03-01", "x ^0000-03-01", "^0000-03-01: the year must be 1 to 9999"],
        ["x >later", "x >later", ">later: status must be >next or >someday"],
        ["x #a\u0007b", "x #a\u0007b", "#a\u0007b: labels must not contain control characters"],
        ['ask ~"the vendor', 'ask ~"the vendor', '~"the: missing closing quote'],
        ['x ~""', 'x ~""', '~"": waiting needs a name'],
        ['x ~"   " y', 'x ~" " y', '~"   ": waiting needs a name'],
      ];
      for (const [text, title, warning] of rows) {
        const d = await parse(text);
        expect(d?.title, text).toBe(title);
        expect(d?.warnings, text).toEqual([warning]);
        expect(d?.tokens.map((tk) => tk[3]), text).toEqual([false]);
        expect(d?.tokens[0]?.[4], text).toBe(warning);
        expect([d?.priority, d?.due, d?.waiting, d?.status, d?.labels], text).toEqual([Priority.NONE, undefined, undefined, Status.INBOX, []]);
      }
      // The groundwork probe: the last ^ does not parse, so it is a title word.
      expect(await parse('call ^fri +2 #a ~"the vendor" >next ^bogus')).toEqual({
        title: "call ^bogus",
        priority: Priority.HIGH,
        due: "2026-10-23",
        labels: ["a"],
        waiting: "the vendor",
        status: Status.NEXT,
        tokens: [
          [5, 9, K.DUE_DATE, true, ""],
          [10, 12, K.PRIORITY, true, ""],
          [13, 15, K.LABEL, true, ""],
          [16, 29, K.WAITING, true, ""],
          [30, 35, K.STATUS, true, ""],
          [36, 42, K.DUE_DATE, false, "^bogus: not a date"],
        ],
        warnings: ["^bogus: not a date"],
      });
      // A failed token is a title word, so the text still has a title.
      expect(await parse("+9 +1")).toMatchObject({ title: "+9", priority: Priority.HIGHEST, warnings: ["+9: priority must be +1 to +5"] });
    });

    it("takes \\ as an escape before a sigil only, and ~\"…\" up to a quote before whitespace", async () => {
      const titles: [string, string][] = [
        ["\\#1 fan", "#1 fan"],
        ["\\+2 \\^fri \\~dana \\>next", "+2 ^fri ~dana >next"],
        ["\\foo", "\\foo"],
        ["\\\\#x", "\\\\#x"],
        ["a\\#b", "a\\#b"],
        ['\\~"the vendor"', '~"the vendor"'],
        ["a + b ^ c # d ~ e > f", "a + b ^ c # d ~ e > f"],
      ];
      for (const [text, title] of titles) expect(await parse(text), text).toMatchObject({ title, tokens: [], warnings: [] });
      expect(await parse("\\#1 fan +1")).toMatchObject({ title: "#1 fan", priority: Priority.HIGHEST, tokens: [[8, 10, K.PRIORITY, true, ""]] });

      expect(await parse('ping ~"the vendor" re invoice')).toMatchObject({ title: "ping re invoice", waiting: "the vendor", tokens: [[5, 18, K.WAITING, true, ""]] });
      expect(await parse('x ~"  spaced   out " y')).toMatchObject({ title: "x y", waiting: "spaced   out" });
      expect(await parse('~"a"b c" d')).toMatchObject({ title: "d", waiting: 'a"b c', tokens: [[0, 8, K.WAITING, true, ""]] });
      expect(await parse('x ~dana"')).toMatchObject({ waiting: 'dana"' });
      expect(await parse('ask ~"the vendor"x')).toMatchObject({ title: 'ask ~"the vendor"x', tokens: [[4, 9, K.WAITING, false, '~"the: missing closing quote']] });
    });

    it("counts offsets in code points through emoji, CJK and combining marks", async () => {
      expect(await parse("🎉 party ^fri")).toMatchObject({ title: "🎉 party", due: "2026-10-23", tokens: [[8, 12, K.DUE_DATE, true, ""]] });
      expect(await parse("日本語 #タグ 🎉 +1")).toMatchObject({
        title: "日本語 🎉",
        labels: ["タグ"],
        tokens: [
          [4, 7, K.LABEL, true, ""],
          [10, 12, K.PRIORITY, true, ""],
        ],
      });
      expect(await parse('ñ ~"José Ñ" ok')).toMatchObject({ waiting: "José Ñ", tokens: [[2, 11, K.WAITING, true, ""]] });
      // A family emoji is 5 code points (3 people, 2 joiners); e + U+0301 is 2.
      expect(await parse("👩‍👩‍👧 café #🏷️ ^tom")).toMatchObject({
        title: "👩‍👩‍👧 café",
        labels: ["🏷️"],
        tokens: [
          [12, 15, K.LABEL, true, ""],
          [16, 20, K.DUE_DATE, true, ""],
        ],
      });
      // Unicode White_Space separates words (NEL, ideographic space); BOM does not.
      expect(await parse("a\u0085b　+1")).toMatchObject({ title: "a b", priority: Priority.HIGHEST, tokens: [[4, 6, K.PRIORITY, true, ""]] });
      expect(await parse("a﻿+1")).toMatchObject({ title: "a﻿+1", tokens: [] });
    });

    it("keeps text without title words as typed, applying no token", async () => {
      expect(await parse("+1 ^fri")).toEqual({
        title: "+1 ^fri",
        priority: Priority.NONE,
        due: undefined,
        labels: [],
        waiting: undefined,
        status: Status.INBOX,
        tokens: [
          [0, 2, K.PRIORITY, false, "+1: kept as title text"],
          [3, 7, K.DUE_DATE, false, "^fri: kept as title text"],
        ],
        warnings: ["+1: kept as title text", "^fri: kept as title text", "no title words: the text was kept as typed"],
      });
      expect(await parse("  #a \t >next  +1 +2 ")).toMatchObject({ title: "#a >next +1 +2", labels: [], status: Status.INBOX, priority: Priority.NONE });
      expect(await parse('~"the vendor"')).toMatchObject({ title: '~"the vendor"', waiting: undefined });
      // Blank text is an empty draft, not a failure.
      for (const blank of ["", "   ", " \t\n ", "\u0085"]) expect(await parse(blank), JSON.stringify(blank)).toMatchObject({ title: "", tokens: [], warnings: [] });
    });

    it("lets the last token of a kind win, and applies every label once", async () => {
      expect(await parse("x +1 +2 +3")).toMatchObject({
        priority: Priority.MEDIUM,
        warnings: ["+1: overridden by +3", "+2: overridden by +3"],
        tokens: [
          [2, 4, K.PRIORITY, false, "+1: overridden by +3"],
          [5, 7, K.PRIORITY, false, "+2: overridden by +3"],
          [8, 10, K.PRIORITY, true, ""],
        ],
      });
      expect(await parse("x ^tom ^mon")).toMatchObject({ due: "2026-10-26", warnings: ["^tom: overridden by ^mon"] });
      expect(await parse('x ~a ~"b c"')).toMatchObject({ waiting: "b c", warnings: ['~a: overridden by ~"b c"'] });
      expect(await parse("x >next >someday")).toMatchObject({ status: Status.SOMEDAY, warnings: [">next: overridden by >someday"] });
      // A failed token overrides nothing; warnings come in text order.
      expect(await parse("+9 x +1 +2 ^nope")).toMatchObject({
        title: "+9 x ^nope",
        priority: Priority.HIGH,
        warnings: ["+9: priority must be +1 to +5", "+1: overridden by +2", "^nope: not a date"],
      });
      expect(await parse("x #a #b #a #A")).toMatchObject({ labels: ["a", "b", "A"], warnings: [] });
    });

    it("resolves ^ dates on the reference day in the daemon's zone, across weeks, months and years", async () => {
      const rows: [Timestamp, string, string][] = [
        // Thursday 2026-10-22, which is still Wednesday in UTC.
        [THU, "tod", "2026-10-22"],
        [THU, "tom", "2026-10-23"],
        [THU, "thu", "2026-10-29"],
        [THU, "fri", "2026-10-23"],
        [THU, "mon", "2026-10-26"],
        [THU, "eow", "2026-10-23"],
        [THU, "+1w", "2026-10-29"],
        [THU, "+1m", "2026-11-22"],
        [THU, "eom", "2026-10-31"],
        [THU, "oct22", "2026-10-22"],
        [THU, "oct21", "2027-10-21"],
        [THU, "feb29", "2028-02-29"],
        [at(10, 23), "eow", "2026-10-23"],
        [at(10, 23), "fri", "2026-10-30"],
        [at(10, 24), "eow", "2026-10-30"],
        [at(10, 24), "sun", "2026-10-25"],
        [at(10, 25), "mon", "2026-10-26"],
        [at(10, 25), "sun", "2026-11-01"],
        [at(10, 25), "eow", "2026-10-30"],
        // The last minute of a Sunday and the first of a Monday.
        [at(10, 25, 2026, 23, 59), "tom", "2026-10-26"],
        [at(10, 26, 2026, 0, 1), "tod", "2026-10-26"],
        [at(10, 26, 2026, 0, 1), "mon", "2026-11-02"],
        // New Year's Eve, a Thursday: ISO week 2026-W53 runs into 2027.
        [at(12, 31), "fri", "2027-01-01"],
        [at(12, 31), "eow", "2027-01-01"],
        [at(12, 31), "tom", "2027-01-01"],
        [at(12, 31), "jan1", "2027-01-01"],
        [at(12, 31), "dec31", "2026-12-31"],
        [at(12, 31), "eom", "2026-12-31"],
        [at(12, 31), "+2m", "2027-02-28"],
        [at(1, 31, 2028), "+1m", "2028-02-29"],
      ];
      for (const [ref, expr, want] of rows) expect((await parse(`x ^${expr}`, ref))?.due, `${expr} at ${timestampDate(ref).toISOString()}`).toBe(want);
    });

    it("refuses text over 64 KiB of UTF-8, in ParseCapture and CaptureItem", async () => {
      const longest = "é".repeat(MAX_CAPTURE_BYTES / 2);
      expect((await parse(longest))?.title).toBe(longest);
      expect(await rejected(parseCapture(t.items, `${longest}x`, { referenceTime: THU }))).toEqual(["text", "is longer than 64 KiB"]);
      expect(await rejected(capture(`${longest}x`))).toEqual(["text", "is longer than 64 KiB"]);
    });

    it("CaptureItem applies the draft on the reference day and returns it", async () => {
      const text = "call dentist ^fri +2 #home ~dana >next";
      const res = await capture(text);
      expect(shown(res.draft)).toEqual(await parse(text));
      const item = res.item!;
      expect(item).toMatchObject({ title: "call dentist", priority: Priority.HIGH, labels: ["home"], status: Status.NEXT, createdVia: CreatedVia.TUI, createdAt: THU });
      expect(iso(item.dueDate)).toBe("2026-10-23");
      expect(item.waiting?.on).toMatchObject(freeText("dana"));
      expect([item.waiting?.reason, iso(item.waiting?.since), item.waiting?.followUpOn]).toEqual(["", "2026-10-22", undefined]);
      expect((await t.items.getItem({ id: item.id })).item?.title).toBe("call dentist");

      // Late on a Sunday in the zone: waiting since that Sunday.
      const late = await capture("ask ~dana ^tom", { referenceTime: at(10, 25, 2026, 23, 59) });
      expect([iso(late.item?.waiting?.since), iso(late.item?.dueDate)]).toEqual(["2026-10-25", "2026-10-26"]);
      expect((await capture("later >someday")).item?.status).toBe(Status.SOMEDAY);

      // Warnings do not fail a capture; text without title words is kept.
      const kept = await capture("+1 ^fri");
      expect([kept.item?.title, kept.item?.priority, kept.item?.dueDate]).toEqual(["+1 ^fri", Priority.NONE, undefined]);
      expect(kept.draft?.warnings.at(-1)).toBe("no title words: the text was kept as typed");
      const bogus = await capture("x ^bogus +7");
      expect(bogus.item?.title).toBe("x ^bogus +7");
      expect(bogus.draft?.warnings).toEqual(["^bogus: not a date", "+7: priority must be +1 to +5"]);
    });

    it("CaptureItem replays by request_id and by spool_id, parsing on the day the Item was created", async () => {
      const requestId = ulid();
      const first = await capture("once ^tom", { requestId });
      // No reference_time now: the draft is still parsed on Thursday.
      const again = await t.items.captureItem({ requestId, text: "once ^tom", createdVia: CreatedVia.TUI });
      expect([again.item?.id, again.result]).toEqual([first.item?.id, first.result]);
      expect(shown(again.draft)).toEqual(shown(first.draft));
      expect(iso(again.draft?.dueDate)).toBe("2026-10-23");

      const spoolId = ulid();
      const spooled = await capture("spooled ^tom", { spoolId });
      const imported = await t.items.captureItem({ requestId: ulid(), text: "spooled ^tom", createdVia: CreatedVia.CLI, spoolId: spoolId.toLowerCase() });
      expect([imported.item?.id, imported.result]).toEqual([spooled.item?.id, spooled.result]);
      expect(imported.item?.createdVia).toBe(CreatedVia.TUI);
      expect(iso(imported.draft?.dueDate)).toBe("2026-10-23");
      // An empty spool_id is none.
      expect((await capture("no spool", { spoolId: "" })).item?.title).toBe("no spool");
      expect(await rejected(capture("x", { spoolId: "nope" }))).toEqual(["spool_id", "must be a ULID"]);
    });

    it("CaptureItem checks its text and created_via as the daemon does", async () => {
      for (const blank of [" ", "\t\n", "\u0085"]) expect(await rejected(capture(blank)), JSON.stringify(blank)).toEqual(["text", "must not be empty"]);
      expect(await rejected(capture("x", { createdVia: CreatedVia.UNSPECIFIED }))).toEqual(["created_via", "value must not be UNSPECIFIED"]);
      expect(await rejected(capture("x", { createdVia: 99 as CreatedVia }))).toEqual(["created_via", "unknown enum value 99"]);
      for (const via of [CreatedVia.REMOTE_IMPORT, CreatedVia.RESTORE, CreatedVia.SPLIT, CreatedVia.RECURRENCE]) {
        expect(await rejected(capture("x", { createdVia: via }))).toEqual(["created_via", "must be TUI, CLI or MCP; the other values are reserved for the daemon"]);
      }
      for (const via of [CreatedVia.CLI, CreatedVia.MCP]) expect((await capture("x", { createdVia: via })).item?.createdVia).toBe(via);
    });

    it("UpdateItem waits since today in the daemon's zone, and a nested waiting path changes only its field", async () => {
      const id = (await capture("chase")).item!.id;
      const set = (waiting: Parameters<typeof updateItem>[1]["waiting"], paths: string[]) => updateItem(t.items, { id, ...(waiting && { waiting }) }, paths);
      const get = async (): Promise<Item> => (await t.items.getItem({ id })).item!;
      const today = () => formatDay(dayIn(t.timeZone, Date.now()));

      // As the w prompt writes it: the person, trimmed, since today.
      const before = today();
      const res = await set({ on: freeText("  dana  ") }, ["waiting"]);
      expect([before, today()]).toContain(iso(res.item?.waiting?.since));
      expect(res.item?.waiting?.on).toMatchObject(freeText("dana"));
      expect(res.item?.waiting?.reason).toBe("");
      const since = res.item!.waiting!.since;

      // A follow-up alone keeps the person and the age.
      await set({ on: freeText("ignored"), followUpOn: { year: 2026, month: 11, day: 2 } }, ["waiting.follow_up_on"]);
      let w = (await get()).waiting;
      expect([w?.on, w?.since, iso(w?.followUpOn)]).toEqual([expect.objectContaining(freeText("dana")), since, "2026-11-02"]);
      await set({ reason: "quote", since: { year: 2026, month: 10, day: 1 } }, ["waiting.reason", "waiting.since"]);
      w = (await get()).waiting;
      expect([w?.reason, iso(w?.since), iso(w?.followUpOn)]).toEqual(["quote", "2026-10-01", "2026-11-02"]);
      await set({ on: freeText(" sam ") }, ["waiting.on"]);
      await set(undefined, ["waiting.follow_up_on"]);
      w = (await get()).waiting;
      expect([w?.on, w?.reason, w?.followUpOn]).toEqual([expect.objectContaining(freeText("sam")), "quote", undefined]);

      expect(await rejected(set(undefined, ["waiting.on"]))).toEqual(["item.waiting.on", "clear waiting instead"]);
      expect(await rejected(set({ on: freeText("x") }, ["waiting.since"]))).toEqual(["item.waiting.since", "cannot be cleared"]);
      expect(await rejected(set({ on: freeText(" \t ") }, ["waiting"]))).toEqual(["item.waiting.on", "free text must not be empty"]);
      expect(await rejected(set({ reason: "no one" }, ["waiting"]))).toEqual(["item.waiting.on", "required field is missing"]);
      // A given since is kept.
      const given = await set({ on: freeText("kim"), since: { year: 2026, month: 9, day: 30 } }, ["waiting"]);
      expect(iso(given.item?.waiting?.since)).toBe("2026-09-30");

      // Cleared, its fields cannot be set.
      expect((await set(undefined, ["waiting"])).item?.waiting).toBeUndefined();
      for (const path of ["waiting.on", "waiting.reason", "waiting.since", "waiting.follow_up_on"]) {
        expect(await rejected(set({ on: freeText("x"), reason: "r", since: { year: 2026, month: 10, day: 1 } }, [path]))).toEqual([
          `item.${path}`,
          "cannot be set while waiting is unset",
        ]);
      }
      const batch = batchMutate(t.items, [{ kind: { case: "updateItem", value: { item: { id, waiting: { reason: "r" } }, updateMask: { paths: ["waiting.reason"] } } } }]);
      expect(await rejected(batch)).toEqual(["mutations[0].update_item.item.waiting.reason", "cannot be set while waiting is unset"]);
      // A batch's waiting without since waits since today too.
      const batched = await batchMutate(t.items, [
        { kind: { case: "updateItem", value: { item: { id, waiting: { on: freeText("lee") } }, updateMask: { paths: ["waiting"] } } } },
      ]);
      expect([before, today()]).toContain(iso(batched.items[0]?.waiting?.since));
    });
  });
}
