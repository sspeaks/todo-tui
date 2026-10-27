import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { parseCaptureText } from "../src/capture.js";
import { dayNumber } from "../src/dates.js";
import { DateSchema } from "../src/gen/google/type/date_pb.js";
import { PersonRefSchema } from "../src/gen/todo/v1/common_pb.js";
import { Priority, Status } from "../src/gen/todo/v1/item_pb.js";
import { type CaptureDraft, CaptureDraftSchema, CaptureTokenKind as K, CaptureTokenSchema } from "../src/gen/todo/v1/item_service_pb.js";

// The cases of crates/todo-core/src/capture.rs, whose parser FakeDaemon's
// ParseCapture and CaptureItem port. test/conformance/capture.ts checks the
// port against the daemon over the API.

/** A Thursday. */
const TODAY = dayNumber(2026, 10, 22);

const parse = (text: string) => parseCaptureText(text, TODAY);

/** A draft of plain title text: default fields and no tokens. */
const fromTitle = (title: string) => create(CaptureDraftSchema, { title, priority: Priority.NONE, status: Status.INBOX });

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
        tokens: [token(13, 17, K.DUE_DATE), token(18, 20, K.PRIORITY), token(21, 26, K.LABEL)],
      }),
    );
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
    const text = "a + b ^ c # d ~ e > f";
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
      "\\+1 + ++ ^ ^^ # ## ~ ~~ > >>",
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
});
