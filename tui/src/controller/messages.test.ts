import { BadRequestSchema, Code, ConnectError, ErrorInfoSchema, create, OpSummarySchema, RedoResponseSchema, UndoResponseSchema, UndoSkipSchema } from "@todo-tui/client";
import { describe, expect, it } from "vitest";
import { DaemonUnavailable, errorText, undoText } from "./messages.js";

const info = (code: Code, reason: string, message = "x") => new ConnectError(message, code, undefined, [{ desc: ErrorInfoSchema, value: { reason, domain: "todo" } }]);

describe("error toasts (m4_arch §3.2)", () => {
  it("maps the reasons the TUI meets to a sentence", () => {
    expect(errorText(info(Code.FailedPrecondition, "ITEM_DELETED"), "call dentist")).toBe("“call dentist” was deleted elsewhere");
    expect(errorText(info(Code.NotFound, "ITEM_NOT_FOUND"), "x".repeat(60))).toBe(`“${"x".repeat(39)}…” was deleted elsewhere`);
    expect(errorText(info(Code.NotFound, "ITEM_NOT_FOUND"))).toBe("The Item was deleted elsewhere");
    expect(errorText(info(Code.FailedPrecondition, "PARENT_DELETED"))).toBe("Undelete the parent first");
    const bad = new ConnectError("bad", Code.InvalidArgument, undefined, [
      { desc: BadRequestSchema, value: { fieldViolations: [{ field: "item.title", description: "must be one line" }] } },
    ]);
    expect(errorText(bad)).toBe("Invalid title: must be one line");
    expect(errorText(new DaemonUnavailable(new Error("ECONNREFUSED")))).toBe("Not saved: daemon unavailable");
    expect(errorText(new ConnectError("gone", Code.Unavailable))).toBe("Not saved: daemon unavailable");
    expect(errorText(new ConnectError("slow", Code.DeadlineExceeded))).toBe("The daemon did not answer; the change may not be saved");
    expect(errorText(new ConnectError("disk \x1b[31mfull", Code.Internal))).toBe("Internal: disk full");
    expect(errorText(new Error("weird"))).toBe("Unknown: weird");
  });

  it("words undo and redo as the CLI does", () => {
    const entry = create(OpSummarySchema, { opId: "1", description: 'mark done "x"' });
    const item = { $typeName: "todo.v1.Item" } as never;
    expect(undoText(create(UndoResponseSchema, {}), false)).toBe("Nothing to undo");
    expect(undoText(create(RedoResponseSchema, {}), true)).toBe("Nothing to redo");
    expect(undoText(create(UndoResponseSchema, { entry, items: [item] }), false)).toBe('Undid: mark done "x"');
    expect(undoText(create(RedoResponseSchema, { entry, items: [item] }), true)).toBe('Redid: mark done "x"');
    expect(undoText(create(UndoResponseSchema, { entry }), false)).toBe('Could not undo: mark done "x" (everything it changed has changed since)');
    const skipped = [create(UndoSkipSchema, {}), create(UndoSkipSchema, {})];
    expect(undoText(create(UndoResponseSchema, { entry, items: [item], skipped }), false)).toBe('Undid: mark done "x" · 2 changes kept (changed since)');
  });
});
