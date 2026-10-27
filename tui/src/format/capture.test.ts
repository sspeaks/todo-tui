import { CaptureTokenKind, create, CaptureDraftSchema, CaptureTokenSchema, Priority, Status } from "@todo-tui/client";
import { describe, expect, it } from "vitest";
import { lineOf } from "../keys/line-editor.js";
import { dayNumber } from "./dates.js";
import { captureSummary, codePointIndexes, lineSegments, tokenSpans } from "./capture.js";

const token = (start: number, end: number, kind: CaptureTokenKind, applied = true, warning = "") =>
  create(CaptureTokenSchema, { start, end, kind, applied, warning });

describe("the quick-add preview", () => {
  it("turns code-point offsets into JS string indexes (hard case 41)", () => {
    const text = "🎉 party ^fri";
    expect(codePointIndexes("a🎉b")).toEqual([0, 1, 3, 4]);
    // The daemon counts 🎉 as one: ^fri is code points 8–12, but JS indexes 9–13.
    const [span] = tokenSpans(text, [token(8, 12, CaptureTokenKind.DUE_DATE)]);
    expect(text.slice(span!.start, span!.end)).toBe("^fri");
    // A token past the text (a stale draft) is dropped.
    expect(tokenSpans("ab", [token(1, 5, CaptureTokenKind.LABEL)])).toEqual([]);
  });

  it("draws the line as coloured runs, unapplied red, the cursor's cell on its own", () => {
    const text = "🎉 x +2 ^nope";
    const spans = tokenSpans(text, [token(4, 6, CaptureTokenKind.PRIORITY), token(7, 12, CaptureTokenKind.DUE_DATE, false, "^nope: not a date")]);
    expect(lineSegments(lineOf(text), 40, spans)).toEqual([
      { text: "🎉 x " },
      { text: "+2", color: "magenta" },
      { text: " " },
      { text: "^nope", color: "red" },
      { text: " ", cursor: true },
    ]);
    expect(lineSegments({ text, cursor: 4 }, 40, spans)).toEqual([
      { text: "🎉 x " },
      { text: "+", color: "magenta", cursor: true },
      { text: "2", color: "magenta" },
      { text: " " },
      { text: "^nope", color: "red" },
    ]);
    // A window around the cursor; control characters left out.
    expect(lineSegments(lineOf("abcdef"), 3, [])).toEqual([{ text: "ef" }, { text: " ", cursor: true }]);
    expect(lineSegments(lineOf("a\x1bb"), 10, [])).toEqual([{ text: "ab" }, { text: " ", cursor: true }]);
  });

  it("sums up the draft as `→ title · +2 · ^Fri Oct 30 · #a · ⧗dana · >next`", () => {
    const today = dayNumber(2026, 10, 26);
    const draft = create(CaptureDraftSchema, {
      title: "call",
      priority: Priority.HIGH,
      dueDate: { year: 2026, month: 10, day: 30 },
      labels: ["a", "b"],
      waitingOn: { ref: { case: "freeText", value: "dana" } },
      status: Status.NEXT,
    });
    expect(captureSummary(draft, today)).toBe("→ call · +2 · ^Fri Oct 30 · #a #b · ⧗dana · >next");
    expect(captureSummary(create(CaptureDraftSchema, { title: "x\x1b[31m", status: Status.INBOX }), today)).toBe("→ x");
    expect(captureSummary(create(CaptureDraftSchema, { title: "x", status: Status.SOMEDAY, dueDate: { year: 2027, month: 1, day: 5 } }), today)).toBe(
      "→ x · ^Tue Jan 5 2027 · >someday",
    );
  });
});
