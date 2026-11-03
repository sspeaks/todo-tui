import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { parseCaptureText } from "../src/capture.js";
import { dayNumber } from "../src/dates.js";
import { DateSchema } from "../src/gen/google/type/date_pb.js";
import { PersonRefSchema } from "../src/gen/todo/v1/common_pb.js";
import { Energy, Priority, Status, Weekday } from "../src/gen/todo/v1/item_pb.js";
import { type CaptureDraft, CaptureDraftSchema, CaptureTokenKind as K, CaptureTokenSchema } from "../src/gen/todo/v1/item_service_pb.js";
import { type Rule, ruleToProto } from "../src/recurrence.js";

// The cases of crates/todo-core/src/capture.rs, whose parser FakeDaemon's
// ParseCapture and CaptureItem port. test/conformance/capture.ts checks the
// port against the daemon over the API.

/** A Thursday. */
const TODAY = dayNumber(2026, 10, 22);

const parse = (text: string) => parseCaptureText(text, TODAY);

/** A draft of plain title text: default fields and no tokens. */
const fromTitle = (title: string) => create(CaptureDraftSchema, { title, priority: Priority.NONE, status: Status.INBOX, energy: Energy.NONE });

const token = (start: number, end: number, kind: K) => create(CaptureTokenSchema, { start, end, kind, applied: true, warning: "" });
const failed = (start: number, end: number, kind: K, warning: string) => create(CaptureTokenSchema, { start, end, kind, applied: false, warning });
const date = (year: number, month: number, day: number) => create(DateSchema, { year, month, day });
const freeText = (name: string) => create(PersonRefSchema, { ref: { case: "freeText", value: name } });

describe("parseCaptureText", () => {
  it("parses the design example", () => {
    expect(parse("call dentist ^fri +2 #home")).toEqual(
      create(CaptureDraftSchema, {
        title: "call dentist",
        priority: Priority.HIGH,
        dueDate: date(2026, 10, 23),
        labels: ["home"],
        status: Status.INBOX,
        energy: Energy.NONE,
        tokens: [token(13, 17, K.DUE_DATE), token(18, 20, K.PRIORITY), token(21, 26, K.LABEL)],
      }),
    );
  });

  it("parses %energy, case-insensitive, keeping anything else in the title", () => {
    for (const [text, want] of [
      ["x %low", Energy.LOW],
      ["x %MED", Energy.MEDIUM],
      ["x %medium", Energy.MEDIUM],
      ["x %High", Energy.HIGH],
    ] as const) {
      const d = parse(text);
      expect(d.energy, text).toBe(want);
      expect(d.title, text).toBe("x");
      expect(d.tokens, text).toEqual([token(2, text.length, K.ENERGY)]);
    }
    const huge = parse("x %huge");
    expect(huge.energy).toBe(Energy.NONE);
    expect(huge.title).toBe("x %huge");
    expect(huge.warnings).toEqual(["%huge: not an energy level: use %low, %med or %high"]);
    expect(parse("x %low %high").energy).toBe(Energy.HIGH);
    expect(parse("grow 100% % done").tokens).toEqual([]);
    expect(parse("x \\%low").title).toBe("x %low");
  });

  it("parses each sigil", () => {
    const priorities = [
      ["+1", Priority.HIGHEST],
      ["+2", Priority.HIGH],
      ["+3", Priority.MEDIUM],
      ["+4", Priority.LOW],
      ["+5", Priority.LOWEST],
    ] as const;
    for (const [sigil, want] of priorities) {
      const draft = parse(`x ${sigil}`);
      expect(draft.priority, sigil).toBe(want);
      expect(draft.title).toBe("x");
      expect(draft.tokens).toEqual([token(2, 4, K.PRIORITY)]);
    }

    let draft = parse("x ^tomorrow");
    expect(draft.dueDate).toEqual(date(2026, 10, 23));
    expect(draft.tokens).toEqual([token(2, 11, K.DUE_DATE)]);
    expect(parse("x ^+3d").dueDate).toEqual(date(2026, 10, 25));
    expect(parse("x ^2026-12-01").dueDate).toEqual(date(2026, 12, 1));

    draft = parse("x #Auth");
    expect(draft.labels).toEqual(["Auth"]);
    expect(draft.tokens).toEqual([token(2, 7, K.LABEL)]);
    expect(parse("x ##a#b").labels).toEqual(["#a#b"]);
    // A label with a control character stays in the title.
    draft = parse("x #a\u0007b #c");
    expect(draft.labels).toEqual(["c"]);
    expect(draft.title).toBe("x #a\u0007b");
    expect(draft.warnings).toEqual(["#a\u0007b: labels must not contain control characters"]);

    draft = parse("x ~dana");
    expect(draft.waitingOn).toEqual(freeText("dana"));
    expect(draft.tokens).toEqual([token(2, 7, K.WAITING)]);
    expect(parse('x ~dana"').waitingOn).toEqual(freeText('dana"'));

    for (const [sigil, want] of [
      [">next", Status.NEXT],
      [">NEXT", Status.NEXT],
      [">someday", Status.SOMEDAY],
      [">SomeDay", Status.SOMEDAY],
    ] as const) {
      const d = parse(`x ${sigil}`);
      expect(d.status, sigil).toBe(want);
      expect(d.title).toBe("x");
      expect(d.tokens[0]?.kind).toBe(K.STATUS);
    }
  });

  it("gives plain text the defaults", () => {
    const draft = parse("call dentist");
    expect(draft).toEqual(fromTitle("call dentist"));
    expect(draft.waitingOn).toBeUndefined();
    expect(draft.dueDate).toBeUndefined();
  });

  it("unescapes \\ before a sigil only", () => {
    const rows = [
      ["\\#1 fan", "#1 fan"],
      ["\\+2 \\^fri \\~dana \\>next", "+2 ^fri ~dana >next"],
      ["\\@daily \\@x", "@daily @x"],
      ["\\~dana", "~dana"],
      ["\\foo", "\\foo"],
      ["\\\\#x", "\\\\#x"],
      ["\\", "\\"],
      ["a\\#b", "a\\#b"],
      ['\\~"the vendor"', '~"the vendor"'],
    ];
    for (const [text, title] of rows) expect(parse(text!), text).toEqual(fromTitle(title!));
    const draft = parse("\\#1 fan +1");
    expect(draft.title).toBe("#1 fan");
    expect(draft.priority).toBe(Priority.HIGHEST);
    expect(draft.tokens).toEqual([token(8, 10, K.PRIORITY)]);
  });

  it("keeps bare sigils as title text", () => {
    const text = "a + b ^ c # d ~ e > f @ g";
    expect(parse(text)).toEqual(fromTitle(text));
    expect(parse("+")).toEqual(fromTitle("+"));
  });

  it("parses a quoted ~ up to the first quote before whitespace or the end", () => {
    let draft = parse('ping ~"the vendor" re invoice');
    expect(draft.title).toBe("ping re invoice");
    expect(draft.waitingOn).toEqual(freeText("the vendor"));
    expect(draft.tokens).toEqual([token(5, 18, K.WAITING)]);

    draft = parse('x ~"  spaced   out " y');
    expect(draft.waitingOn).toEqual(freeText("spaced   out"));
    expect(draft.title).toBe("x y");

    draft = parse('~"a"b c" d');
    expect(draft.waitingOn).toEqual(freeText('a"b c'));
    expect(draft.title).toBe("d");
    expect(draft.tokens).toEqual([token(0, 8, K.WAITING)]);

    draft = parse('x ~"dana"');
    expect(draft.waitingOn).toEqual(freeText("dana"));
    expect(draft.tokens).toEqual([token(2, 9, K.WAITING)]);
  });

  it("keeps a quoted ~ that does not parse in the title", () => {
    let draft = parse('ask ~"the vendor');
    expect(draft.title).toBe('ask ~"the vendor');
    expect(draft.waitingOn).toBeUndefined();
    expect(draft.tokens).toEqual([failed(4, 9, K.WAITING, '~"the: missing closing quote')]);
    expect(draft.warnings).toEqual(['~"the: missing closing quote']);

    draft = parse('ask ~"the vendor"x');
    expect(draft.title).toBe('ask ~"the vendor"x');
    expect(draft.tokens[0]?.end).toBe(9);
    expect(draft.tokens[0]?.applied).toBe(false);

    draft = parse('x ~" y');
    expect(draft.title).toBe('x ~" y');
    expect(draft.warnings).toEqual(['~": missing closing quote']);

    draft = parse('x ~""');
    expect(draft.title).toBe('x ~""');
    expect(draft.tokens).toEqual([failed(2, 5, K.WAITING, '~"": waiting needs a name')]);

    draft = parse('x ~"   " y');
    expect(draft.title).toBe('x ~" " y');
    expect(draft.warnings).toEqual(['~"   ": waiting needs a name']);
    expect(draft.tokens[0]?.end).toBe(8);
  });

  it("lets a later token override an earlier one of its kind", () => {
    let draft = parse("x +1 +2");
    expect(draft.priority).toBe(Priority.HIGH);
    expect(draft.title).toBe("x");
    expect(draft.tokens).toEqual([failed(2, 4, K.PRIORITY, "+1: overridden by +2"), token(5, 7, K.PRIORITY)]);
    expect(draft.warnings).toEqual(["+1: overridden by +2"]);

    draft = parse("x +1 +2 +3");
    expect(draft.priority).toBe(Priority.MEDIUM);
    expect(draft.warnings).toEqual(["+1: overridden by +3", "+2: overridden by +3"]);

    draft = parse("x ^tom ^mon");
    expect(draft.dueDate).toEqual(date(2026, 10, 26));
    expect(draft.warnings).toEqual(["^tom: overridden by ^mon"]);

    draft = parse('x ~a ~"b c"');
    expect(draft.waitingOn).toEqual(freeText("b c"));
    expect(draft.warnings).toEqual(['~a: overridden by ~"b c"']);

    draft = parse("x >next >someday");
    expect(draft.status).toBe(Status.SOMEDAY);
    expect(draft.warnings).toEqual([">next: overridden by >someday"]);

    // A failed token overrides nothing.
    draft = parse("x +1 +9");
    expect(draft.priority).toBe(Priority.HIGHEST);
    expect(draft.title).toBe("x +9");
    expect(draft.warnings).toEqual(["+9: priority must be +1 to +5"]);

    // Kinds do not override each other.
    draft = parse("x +1 ^tom #a ~b >next");
    expect(draft.tokens.every((t) => t.applied)).toBe(true);
    expect(draft.warnings).toEqual([]);
  });

  it("stores a repeated label once", () => {
    const draft = parse("x #a #b #a #A");
    expect(draft.labels).toEqual(["a", "b", "A"]);
    expect(draft.tokens).toHaveLength(4);
    expect(draft.tokens.every((t) => t.applied)).toBe(true);
    expect(draft.warnings).toEqual([]);
  });

  it("keeps a token that does not parse in the title, with a warning", () => {
    const rows = [
      ["fix +7 thing", "fix +7 thing", "+7: priority must be +1 to +5"],
      ["x +0", "x +0", "+0: priority must be +1 to +5"],
      ["x +6", "x +6", "+6: priority must be +1 to +5"],
      ["x +12", "x +12", "+12: priority must be +1 to +5"],
      ["x +x", "x +x", "+x: priority must be +1 to +5"],
      ["x +1x", "x +1x", "+1x: priority must be +1 to +5"],
      ["x ++1", "x ++1", "++1: priority must be +1 to +5"],
      ["x ^someday", "x ^someday", "^someday: not a date"],
      ["x ^feb30", "x ^feb30", "^feb30: february has no day 30"],
      ["x ^^fri", "x ^^fri", "^^fri: not a date"],
      ["x ^0000-03-01", "x ^0000-03-01", "^0000-03-01: the year must be 1 to 9999"],
      ["x >later", "x >later", ">later: status must be >next or >someday"],
      ["x >inbox", "x >inbox", ">inbox: status must be >next or >someday"],
    ];
    for (const [text, title, warning] of rows) {
      const draft = parse(text!);
      expect(draft.title, text).toBe(title);
      expect(draft.warnings, text).toEqual([warning]);
      expect(draft.tokens, text).toHaveLength(1);
      expect(draft.tokens[0]?.applied, text).toBe(false);
      expect(draft.tokens[0]?.warning).toBe(warning);
      expect(draft.priority).toBe(Priority.NONE);
      expect(draft.dueDate).toBeUndefined();
      expect(draft.status).toBe(Status.INBOX);
    }
    // A failed token is a title word, so the text still has a title.
    const draft = parse("+9 +1");
    expect(draft.title).toBe("+9");
    expect(draft.priority).toBe(Priority.HIGHEST);
  });

  it("keeps text without title words as typed", () => {
    let draft = parse("+1 ^fri");
    expect(draft.title).toBe("+1 ^fri");
    expect(draft.priority).toBe(Priority.NONE);
    expect(draft.dueDate).toBeUndefined();
    expect(draft.tokens).toEqual([failed(0, 2, K.PRIORITY, "+1: kept as title text"), failed(3, 7, K.DUE_DATE, "^fri: kept as title text")]);
    expect(draft.warnings).toEqual(["+1: kept as title text", "^fri: kept as title text", "no title words: the text was kept as typed"]);

    draft = parse("  #a \t >next  +1 +2 ");
    expect(draft.title).toBe("#a >next +1 +2");
    expect(draft.labels).toEqual([]);
    expect(draft.status).toBe(Status.INBOX);
    expect(draft.priority).toBe(Priority.NONE);
    expect(draft.tokens.every((t) => !t.applied)).toBe(true);

    draft = parse('~"the vendor"');
    expect(draft.title).toBe('~"the vendor"');
    expect(draft.waitingOn).toBeUndefined();
  });

  it("parses blank text to an empty draft", () => {
    for (const text of ["", "   ", " \t\n "]) expect(parse(text), JSON.stringify(text)).toEqual(fromTitle(""));
  });

  it("collapses the title's whitespace (Unicode White_Space)", () => {
    expect(parse("  call \t dentist\n").title).toBe("call dentist");
    expect(parse("a　b").title).toBe("a b");
    // NEL is White_Space, BOM is not (JS's \s has it the other way round).
    expect(parse("a\u0085b").title).toBe("a b");
    expect(parse("a﻿b").title).toBe("a﻿b");
  });

  it("gives offsets in code points", () => {
    let draft = parse("café ^tom");
    expect(draft.title).toBe("café");
    expect(draft.tokens).toEqual([token(5, 9, K.DUE_DATE)]);

    draft = parse("日本語 #タグ 🎉 +1");
    expect(draft.title).toBe("日本語 🎉");
    expect(draft.labels).toEqual(["タグ"]);
    expect(draft.tokens).toEqual([token(4, 7, K.LABEL), token(10, 12, K.PRIORITY)]);

    draft = parse('ñ ~"José Ñ" ok');
    expect(draft.waitingOn).toEqual(freeText("José Ñ"));
    expect(draft.tokens).toEqual([token(2, 11, K.WAITING)]);
  });

  it("gives warnings in text order", () => {
    const draft = parse("+9 x +1 +2 ^nope");
    expect(draft.title).toBe("+9 x ^nope");
    expect(draft.priority).toBe(Priority.HIGH);
    expect(draft.warnings).toEqual(["+9: priority must be +1 to +5", "+1: overridden by +2", "^nope: not a date"]);
  });

  it("parses hostile text in linear time", () => {
    const started = performance.now();
    // Every `~"` word looks for a closing quote that never comes.
    let draft = parse('~"a '.repeat(40_000));
    expect(draft.tokens).toHaveLength(40_000);
    expect(draft.tokens.every((t) => !t.applied)).toBe(true);
    const labels = Array.from({ length: 40_000 }, (_, i) => ` #${i}`).join("");
    draft = parse(`x${labels}`);
    expect(draft.labels).toHaveLength(40_000);
    expect(draft.labels[39_999]).toBe("39999");
    expect(performance.now() - started).toBeLessThan(10_000);
  });

  it("keeps its invariants for assorted text", () => {
    const texts = [
      "call dentist ^fri +2 #home",
      "+1 +2",
      '~"a b" ~"c',
      "\\+1 + ++ ^ ^^ # ## ~ ~~ > >> @ @@",
      "x @weekly ^fri @3d @nope",
      "  ",
      "x +1 #é",
      '~"" ~"x"y" z',
    ];
    for (const text of texts) {
      const draft: CaptureDraft = parse(text);
      const len = Array.from(text).length;
      expect(draft.title === "", text).toBe(text.trim() === "");
      let lastEnd = 0;
      for (const t of draft.tokens) {
        expect(lastEnd <= t.start && t.start < t.end && t.end <= len, text).toBe(true);
        expect(t.applied, text).toBe(t.warning === "");
        lastEnd = t.end;
      }
      const tokenWarnings = draft.tokens.filter((t) => !t.applied).map((t) => t.warning);
      expect(draft.warnings.slice(0, tokenWarnings.length), text).toEqual(tokenWarnings);
    }
  });

  describe("@ repeat rules", () => {
    const { MONDAY, TUESDAY, WEDNESDAY, THURSDAY, FRIDAY } = Weekday;
    const rule = (r: Rule) => ruleToProto(r);
    const weekly = (days: Weekday[]) => rule({ kind: "weekly", days });

    it("parses a rule and its first occurrence", () => {
      let draft = parse("water plants @mon,thu");
      expect(draft.title).toBe("water plants");
      expect(draft.recurrence).toEqual(weekly([MONDAY, THURSDAY]));
      // Without a due date, the first occurrence on or after today.
      expect(draft.recurrenceFirst).toEqual(date(2026, 10, 22));
      expect(draft.dueDate).toBeUndefined();
      expect(draft.tokens).toEqual([token(13, 21, K.RECURRENCE)]);
      expect(draft.warnings).toEqual([]);

      const rows: [string, Rule, ReturnType<typeof date>][] = [
        ["x @daily", { kind: "daily" }, date(2026, 10, 22)],
        ["x @DAILY", { kind: "daily" }, date(2026, 10, 22)],
        ["x @weekdays", { kind: "weekly", days: [MONDAY, TUESDAY, WEDNESDAY, THURSDAY, FRIDAY] }, date(2026, 10, 22)],
        ["x @weekly", { kind: "weekly", days: [THURSDAY] }, date(2026, 10, 22)],
        ["x @monthly", { kind: "monthly", day: 22 }, date(2026, 10, 22)],
        ["x @15th", { kind: "monthly", day: 15 }, date(2026, 11, 15)],
        ["x @mon", { kind: "weekly", days: [MONDAY] }, date(2026, 10, 26)],
      ];
      for (const [text, want, first] of rows) {
        const d = parse(text);
        expect(d.recurrence, text).toEqual(rule(want));
        expect(d.recurrenceFirst, text).toEqual(first);
        expect(d.title, text).toBe("x");
      }

      // After completion: the first occurrence is the due date, if any.
      draft = parse("x @3d");
      expect(draft.recurrence).toEqual(rule({ kind: "afterCompletion", days: 3 }));
      expect(draft.recurrenceFirst).toBeUndefined();
      draft = parse("x @2w ^fri");
      expect(draft.recurrence).toEqual(rule({ kind: "afterCompletion", days: 14 }));
      expect(draft.recurrenceFirst).toEqual(date(2026, 10, 23));
    });

    it("resolves against the due date in any order", () => {
      const friday = date(2026, 10, 23);
      for (const text of ["x @weekly ^fri", "x ^fri @weekly", "@weekly x ^fri"]) {
        const d = parse(text);
        expect(d.recurrence, text).toEqual(weekly([FRIDAY]));
        expect(d.recurrenceFirst, text).toEqual(friday);
        expect(d.dueDate, text).toEqual(friday);
      }
      for (const text of ["x @monthly ^2026-11-30", "x ^2026-11-30 @monthly"]) {
        const d = parse(text);
        expect(d.recurrence, text).toEqual(rule({ kind: "monthly", day: 30 }));
        expect(d.recurrenceFirst, text).toEqual(date(2026, 11, 30));
      }
      // The winning due date anchors, not an overridden one.
      let draft = parse("x ^tom @weekly ^mon");
      expect(draft.recurrence).toEqual(weekly([MONDAY]));
      // A due date off the schedule: the first scheduled date on or after it.
      draft = parse("x ^fri @mon,wed");
      expect(draft.dueDate).toEqual(friday);
      expect(draft.recurrenceFirst).toEqual(date(2026, 10, 26));
      // A failed due date anchors nothing.
      draft = parse("x ^someday @weekly");
      expect(draft.recurrence).toEqual(weekly([THURSDAY]));
      expect(draft.warnings).toEqual(["^someday: not a date"]);
    });

    it("lets a later rule override an earlier one", () => {
      let draft = parse("x @daily @3d");
      expect(draft.recurrence).toEqual(rule({ kind: "afterCompletion", days: 3 }));
      expect(draft.warnings).toEqual(["@daily: overridden by @3d"]);
      expect(draft.tokens).toEqual([failed(2, 8, K.RECURRENCE, "@daily: overridden by @3d"), token(9, 12, K.RECURRENCE)]);
      // A failed token overrides nothing.
      draft = parse("x @daily @home");
      expect(draft.recurrence).toEqual(rule({ kind: "daily" }));
      expect(draft.title).toBe("x @home");
    });

    it("keeps a rule that does not parse in the title", () => {
      const rows = [
        ["x @home", "@home: not a repeat rule"],
        ["x @@daily", "@@daily: not a repeat rule"],
        ["x @mon,fr", "@mon,fr: fr is not a weekday"],
        ["x @mon,", "@mon,: a day is missing between commas"],
        ["x @22th", "@22th: write 22nd"],
        ["x @32nd", "@32nd: the day of the month must be 1st to 31st"],
        ["x @0d", "@0d: after completion must be 1 to 3650 days"],
        ["x @15", "@15: a number needs d or w, or a day suffix, e.g. 3d, 2w or 15th"],
      ];
      for (const [text, warning] of rows) {
        const draft = parse(text!);
        expect(draft.title, text).toBe(text);
        expect(draft.warnings, text).toEqual([warning]);
        expect(draft.recurrence, text).toBeUndefined();
        expect(draft.recurrenceFirst, text).toBeUndefined();
        expect(draft.tokens[0]?.kind).toBe(K.RECURRENCE);
        expect(draft.tokens[0]?.applied).toBe(false);
      }
    });

    it("keeps @ as title text mid-word, alone or escaped", () => {
      for (const text of ["mail bob@x.com", "meet @", "a @ b", "x@daily"]) expect(parse(text), text).toEqual(fromTitle(text));
      expect(parse("\\@daily standup")).toEqual(fromTitle("@daily standup"));
      // Alone, a rule is kept as the title, like any token.
      let draft = parse("@daily");
      expect(draft.title).toBe("@daily");
      expect(draft.recurrence).toBeUndefined();
      expect(draft.recurrenceFirst).toBeUndefined();
      expect(draft.warnings).toEqual(["@daily: kept as title text", "no title words: the text was kept as typed"]);
      draft = parse("@daily ^fri");
      expect(draft.title).toBe("@daily ^fri");
      expect([draft.recurrence, draft.dueDate]).toEqual([undefined, undefined]);
    });
  });
});
