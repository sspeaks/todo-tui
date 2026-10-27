// Quick-add, `a` (Decision 7): a line at the top of the list with a live
// ParseCapture preview (one request in flight, the latest text winning).
// enter captures and stays open, enter on blank text closes, esc
// discards. A paste of several lines captures each non-blank line in
// order, as `todo add` reads stdin; text over 64 KiB is refused here and
// kept. `A` captures as a child of the selected Item.

import {
  BadRequestSchema,
  Code,
  ConnectError,
  CreatedVia,
  MAX_CAPTURE_BYTES,
  type ParseCaptureResponse,
  Status,
  captureItem,
  isConnectionFailure,
  parseCapture,
  ulid,
} from "@todo-tui/client";
import { singleLine, truncate } from "../format/text.js";
import { editLine, emptyLine, insert, type LineState, lineOf } from "../keys/line-editor.js";
import { isChar, type KeyToken } from "../keys/tokenize.js";
import { type Mode, normalMode } from "../state/types.js";
import type { Controller } from "./controller.js";
import { Latest } from "./latest.js";
import { DaemonUnavailable, errorText } from "./messages.js";
import type { Input } from "./queue.js";
import { single } from "./pipeline.js";

type QuickAddMode = Extract<Mode, { kind: "quickAdd" }>;

/** Bytes of UTF-8, which the daemon's 64 KiB limit counts. */
export const captureBytes = (text: string): number => Buffer.byteLength(text, "utf8");

/** Whether the text is over what CaptureItem takes. */
export const overLimit = (text: string): boolean => captureBytes(text) > MAX_CAPTURE_BYTES;

const lines = (text: string) => text.split(/\r\n?|\n/);

/** The seeded view a new Item of `status` lands in, by seed. */
const SEED_OF: Partial<Record<Status, string>> = { [Status.INBOX]: "inbox", [Status.NEXT]: "next", [Status.SOMEDAY]: "someday" };

/**
 * The lines queued keys (and pastes) would have typed into quick-add, up
 * to an esc or ctrl-c; and how many inputs came from there on, which
 * were not meant for quick-add.
 */
export function typedLines(inputs: readonly Input[]): { lines: string[]; dropped: number } {
  const out: string[] = [];
  let line: LineState = emptyLine;
  let i = 0;
  for (; i < inputs.length; i++) {
    const input = inputs[i] ?? "";
    if (typeof input !== "string") {
      const [first = "", ...more] = lines(input.paste);
      line = insert(line, first);
      for (const next of more) {
        out.push(line.text);
        line = lineOf(next);
      }
      continue;
    }
    if (input === "esc" || input === "ctrl+c") break;
    if (input === "enter") {
      out.push(line.text);
      line = emptyLine;
    } else {
      line = editLine(line, input) ?? line;
    }
  }
  out.push(line.text);
  return { lines: out.filter((l) => l.trim() !== ""), dropped: inputs.length - i };
}

/** Whether a failed capture may still have been saved: no answer came (the daemon down past the retry window, or the deadline). */
export const outcomeUnknown = (err: unknown): boolean =>
  err instanceof DaemonUnavailable || isConnectionFailure(err) || ConnectError.from(err).code === Code.DeadlineExceeded;

/** Whether the daemon refused the capture's parent_id: the parent was deleted (or is gone) since `A`. */
const parentRefused = (err: unknown): boolean =>
  ConnectError.from(err).code === Code.InvalidArgument &&
  ConnectError.from(err)
    .findDetails(BadRequestSchema)
    .some((d) => d.fieldViolations.some((v) => v.field === "parent_id"));

export class QuickAdd {
  private readonly previews: Latest<string, ParseCaptureResponse | undefined>;
  /**
   * The last capture whose outcome is unknown: enter on the same text (and
   * parent) sends its request_id again, which the daemon answers with the
   * Item it made, if it made one, so a capture never lands twice.
   */
  private unknown: { text: string; parentId: string | undefined; requestId: string } | undefined;

  constructor(private readonly c: Controller) {
    this.previews = new Latest(
      (text) =>
        text.trim() === "" || overLimit(text) ? Promise.resolve(undefined) : parseCapture(c.items, text, { timeoutMs: c.timing.rpcTimeoutMs }),
      (text, res) => {
        const { mode } = this.c.state;
        // A response for text since edited is stale: never shown, the newer request follows.
        if (mode.kind !== "quickAdd" || mode.line.text !== text) return;
        this.c.setMode({ ...mode, preview: res?.draft ? { text, draft: res.draft } : undefined });
      },
    );
  }

  open(): void {
    this.c.setMode({ kind: "quickAdd", line: emptyLine });
  }

  /** A: quick-add under the selected Item (never the marked ones). */
  openChild(): void {
    const parent = this.c.selectedItem();
    if (!parent) return this.c.toast("info", "Select an Item to add a subtask to");
    this.c.setMode({ kind: "quickAdd", parentId: parent.id, parentTitle: parent.title, line: emptyLine });
  }

  /** Resolves once the preview has caught up with the text (tests). */
  previewIdle(): Promise<void> {
    return this.previews.idle();
  }

  key(mode: QuickAddMode, key: KeyToken): void | Promise<void> {
    switch (key) {
      case "esc":
      case "ctrl+c":
        return this.c.setMode(normalMode);
      case "enter":
        return this.submit(mode);
    }
    const line = isChar(key) ? insert(mode.line, this.c.charRun(key)) : editLine(mode.line, key);
    if (line) this.edit(mode, line);
  }

  /** One line goes in at the cursor; several are captured one by one, each non-blank one (with what was typed around the cursor). */
  paste(mode: QuickAddMode, text: string): void | Promise<void> {
    const pasted = lines(text);
    if (pasted.length === 1) return this.edit(mode, insert(mode.line, text));
    const all = lines(insert(mode.line, text).text).filter((l) => l.trim() !== "");
    return this.captureLines(all, mode.rest ?? []);
  }

  private edit(mode: QuickAddMode, line: LineState): void {
    this.c.setMode({ ...mode, line });
    this.previews.request(line.text);
  }

  private async submit(mode: QuickAddMode): Promise<void> {
    const { text } = mode.line;
    const rest = mode.rest ?? [];
    if (text.trim() === "") {
      // Blank: closes, unless lines wait from a failed paste.
      if (!rest.length) return this.c.setMode(normalMode);
      return this.show(rest);
    }
    if (overLimit(text)) {
      this.c.toast("error", `Too long to capture: ${captureBytes(text).toLocaleString("en")} bytes, at most ${MAX_CAPTURE_BYTES.toLocaleString("en")}`);
      return this.c.dropQueued();
    }
    return this.captureLines([text], rest);
  }

  /**
   * Captures `todo` in order, showing each line while it saves; then the
   * line holds the next of `rest`, or nothing. On a failure the line that
   * failed stays, and the lines after it wait in `rest`.
   */
  private async captureLines(todo: readonly string[], rest: readonly string[]): Promise<void> {
    for (const [i, text] of todo.entries()) {
      const waiting = [...todo.slice(i + 1), ...rest];
      this.setLines(text, waiting);
      if (overLimit(text)) {
        this.c.toast("error", `Too long to capture: ${captureBytes(text).toLocaleString("en")} bytes, at most ${MAX_CAPTURE_BYTES.toLocaleString("en")}`);
        return this.c.dropQueued();
      }
      if (!(await this.capture(text))) return;
    }
    this.show(rest);
  }

  /** The line after a capture: the next waiting line, or empty. */
  private show(rest: readonly string[]): void {
    const [next = "", ...after] = rest;
    this.setLines(next, after);
  }

  /** The line and the lines waiting; the preview stays while the text does. */
  private setLines(text: string, rest: readonly string[]): void {
    const { mode } = this.c.state;
    if (mode.kind !== "quickAdd") return;
    const same = mode.line.text === text;
    this.c.setMode({
      ...mode,
      line: same ? mode.line : lineOf(text),
      preview: same ? mode.preview : undefined,
      rest: rest.length ? rest : undefined,
    });
    if (!same && text) this.previews.request(text);
  }

  /**
   * One CaptureItem (created_via TUI); says where the Item went when this
   * tab won't show it. The text stays in the line when it fails.
   */
  private async capture(text: string): Promise<boolean> {
    const { mode } = this.c.state;
    const parentId = mode.kind === "quickAdd" ? mode.parentId : undefined;
    const again = this.unknown?.text === text && this.unknown.parentId === parentId ? this.unknown : undefined;
    const requestId = again?.requestId ?? ulid();
    const out = await this.c.pipeline.run({
      call: (opts) => captureItem(this.c.items, text, { ...opts, createdVia: CreatedVia.TUI, ...(parentId !== undefined && { parentId }) }),
      items: single,
      requestId,
    });
    this.unknown = !out.ok && outcomeUnknown(out.err) ? { text, parentId, requestId } : undefined;
    if (!out.ok) {
      const parentGone = parentId !== undefined && parentRefused(out.err);
      this.c.failed(parentGone ? "The parent was deleted — esc, then a, to add at the top level" : errorText(out.err));
      return false;
    }
    const { item, result } = out.res;
    if (item && !this.c.currentIds().includes(item.id)) {
      const top = this.c.store.undoState.getSnapshot().undo;
      const what = top && top.opId === result?.opId ? singleLine(top.description) : `capture "${truncate(singleLine(item.title), 40)}"`;
      const seed = SEED_OF[item.status] ?? "inbox";
      const view = this.c.store.getViews().views.find((v) => v.seed === seed);
      this.c.toast("info", `${what} → ${singleLine(view?.name ?? "Inbox")}`);
    }
    return true;
  }
}
