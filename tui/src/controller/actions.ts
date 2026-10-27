// The one-key actions on the selected Item (Decision 5): x X, the s picker,
// p 1–5/0, z, W, dd with its confirm, and u / ctrl-r. Each runs through the
// pipeline; the toasts follow design 2.8 (only what moves or removes a row,
// and undo), the errors m4_arch §3.2.

import {
  type ArchiveItemResponse,
  ItemField,
  Operator,
  type Priority,
  type RedoResponse,
  Status,
  type UnarchiveItemResponse,
  type UndoResponse,
  archiveItem,
  deleteItem,
  errorReason,
  getUndoState,
  redo,
  setStatus,
  unarchiveItem,
  undo,
  updateItem,
} from "@todo-tui/client";
import { singleLine, truncate } from "../format/text.js";
import type { KeyToken } from "../keys/tokenize.js";
import { type Mode, normalMode, type Target } from "../state/types.js";
import type { Controller } from "./controller.js";
import { errorText, opText, undoText } from "./messages.js";
import { single } from "./pipeline.js";

/** The status picker's choices, in the design's order (2.5): a letter each, and the verb OpSummary uses. */
export const STATUS_CHOICES: readonly { readonly key: string; readonly status: Status; readonly name: string; readonly verb: string }[] = [
  { key: "i", status: Status.INBOX, name: "Inbox", verb: "move to inbox" },
  { key: "n", status: Status.NEXT, name: "Next", verb: "move to next" },
  { key: "p", status: Status.IN_PROGRESS, name: "In progress", verb: "start" },
  { key: "s", status: Status.SOMEDAY, name: "Someday", verb: "move to someday" },
  { key: "d", status: Status.DONE, name: "Done", verb: "mark done" },
  { key: "c", status: Status.CANCELLED, name: "Cancelled", verb: "cancel" },
];

const statusWord = (status: Status): string => STATUS_CHOICES.find((c) => c.status === status)?.name.toLowerCase() ?? "in that status";

/** Direct children, in any status: what dd's confirm counts. */
export const childrenFilter = (id: string) => ({
  allOf: [
    {
      kind: {
        case: "predicate" as const,
        value: { target: ItemField.PARENT_ID, op: Operator.EQ, operands: [{ kind: { case: "value" as const, value: { kind: { case: "itemId" as const, value: id } } } }] },
      },
    },
  ],
  includeDone: true,
  includeArchived: true,
  includeDeferred: true,
});

type Picker = Extract<Mode, { kind: "picker" }>;
type Confirm = Extract<Mode, { kind: "confirm" }>;

export class ItemActions {
  constructor(private readonly c: Controller) {}

  /** The Item a target names, as the store holds it now. */
  private itemOf(target: Target | undefined) {
    const id = target?.ids[0];
    return id === undefined ? undefined : this.c.store.getItem(id);
  }

  /** x / X, and the picker's choice. No toggle: an Item already in that status is left alone. */
  async setStatus(status: Status, target = this.c.target(), picked = false): Promise<void> {
    const item = this.itemOf(target);
    if (!item) return;
    if (item.status === status) return this.c.toast("info", `already ${statusWord(status)}${picked ? "" : "; s to reopen"}`);
    const out = await this.c.pipeline.run({ call: (opts) => setStatus(this.c.items, item.id, status, opts), items: single });
    if (!out.ok) return this.c.failed(errorText(out.err, item.title));
    this.c.toastOp(out.res.result?.opId, opText(STATUS_CHOICES.find((c) => c.status === status)?.verb ?? "change", item.title));
  }

  /** p 1–5, p 0: a plain edit, toasted only when the Item leaves the tab. */
  async setPriority(priority: Priority): Promise<void> {
    const item = this.itemOf(this.c.target());
    if (!item) return;
    const out = await this.c.pipeline.run({ call: (opts) => updateItem(this.c.items, { id: item.id, priority }, ["priority"], opts), items: single });
    if (!out.ok) return this.c.failed(errorText(out.err, item.title));
    this.c.toastIfLeft(item.id, out.res.result?.opId, opText("edit", item.title));
  }

  /** W */
  async clearWaiting(): Promise<void> {
    const item = this.itemOf(this.c.target());
    if (!item) return;
    if (!item.waiting) return this.c.toast("info", "Not waiting on anyone");
    const out = await this.c.pipeline.run({ call: (opts) => updateItem(this.c.items, { id: item.id }, ["waiting"], opts), items: single });
    if (!out.ok) return this.c.failed(errorText(out.err, item.title));
    this.c.toastIfLeft(item.id, out.res.result?.opId, opText("edit", item.title));
  }

  /** z: archives, or unarchives an archived Item. */
  async toggleArchive(): Promise<void> {
    const item = this.itemOf(this.c.target());
    if (!item) return;
    const rpc = item.archived ? unarchiveItem : archiveItem;
    const out = await this.c.pipeline.run<ArchiveItemResponse | UnarchiveItemResponse>({ call: (opts) => rpc(this.c.items, item.id, opts), items: single });
    if (!out.ok) return this.c.failed(errorText(out.err, item.title));
    this.c.toastOp(out.res.result?.opId, opText(item.archived ? "unarchive" : "archive", item.title));
  }

  // --- s ---

  openPicker(): void {
    const target = this.c.target();
    const item = this.itemOf(target);
    if (!target || !item) return;
    const index = Math.max(
      0,
      STATUS_CHOICES.findIndex((c) => c.status === item.status),
    );
    this.c.setMode({ kind: "picker", picker: "status", target, index });
  }

  /** A letter picks; j/k and enter pick too; esc cancels; other keys do nothing. */
  pickerKey(mode: Picker, key: KeyToken): void | Promise<void> {
    const n = STATUS_CHOICES.length;
    switch (key) {
      case "esc":
      case "ctrl+c":
        return this.c.setMode(normalMode);
      case "j":
      case "down":
        return this.c.setMode({ ...mode, index: (mode.index + 1) % n });
      case "k":
      case "up":
        return this.c.setMode({ ...mode, index: (mode.index + n - 1) % n });
      case "enter":
        return this.pick(mode, STATUS_CHOICES[mode.index]?.status);
    }
    const choice = STATUS_CHOICES.find((c) => c.key === key);
    if (choice) return this.pick(mode, choice.status);
  }

  private pick(mode: Picker, status: Status | undefined): Promise<void> | void {
    this.c.setMode(normalMode);
    if (status !== undefined) return this.setStatus(status, mode.target, true);
  }

  // --- dd ---

  /** dd: counts the direct children for the confirm, then asks. */
  async confirmDelete(): Promise<void> {
    const target = this.c.target();
    const item = this.itemOf(target);
    if (!target || !item) return;
    let children: number;
    try {
      const res = await this.c.pipeline.call(({ timeoutMs }) =>
        this.c.items.listItems({ filter: childrenFilter(item.id), pageSize: 1000 }, { ...(timeoutMs !== undefined && { timeoutMs }) }),
      );
      children = res.items.length;
    } catch (err) {
      return this.c.failed(errorText(err, item.title));
    }
    const title = truncate(singleLine(item.title), 40);
    const subtasks = children ? ` and its ${children} subtask${children === 1 ? "" : "s"}` : "";
    this.c.setMode({ kind: "confirm", action: "delete", target, text: `Delete "${title}"${subtasks}? y/N` });
  }

  /** y deletes; any other key cancels, and goes no further. */
  confirmKey(mode: Confirm, key: KeyToken): void | Promise<void> {
    this.c.setMode(normalMode);
    if (key === "y") return this.delete(mode.target);
  }

  private async delete(target: Target): Promise<void> {
    const item = this.itemOf(target);
    if (!item) return;
    const out = await this.c.pipeline.run({ call: (opts) => deleteItem(this.c.items, item.id, opts), items: single });
    if (!out.ok) return this.c.failed(errorText(out.err, item.title));
    this.c.toastOp(out.res.result?.opId, opText("delete", item.title));
  }

  // --- u / ctrl-r ---

  /**
   * u / ctrl-r: undoes the entry the status bar names (expected_op_id),
   * never one that landed since unseen; then selects the first Item it
   * brought back into the tab.
   */
  async undo(isRedo: boolean): Promise<void> {
    const undoState = this.c.store.undoState;
    const entryOf = () => (isRedo ? undoState.getSnapshot().redo : undoState.getSnapshot().undo);
    let entry = entryOf();
    if (!entry) {
      // None known (or not read yet): ask, waiting for the daemon as an action does.
      try {
        const res = await this.c.pipeline.call(({ timeoutMs }) => getUndoState(this.c.items, { ...(timeoutMs !== undefined && { timeoutMs }) }));
        entry = isRedo ? res.redo : res.undo;
      } catch (err) {
        return this.c.failed(errorText(err));
      }
    }
    if (!entry) return this.c.toast("info", isRedo ? "Nothing to redo" : "Nothing to undo");
    const { opId } = entry;
    const out = await this.c.pipeline.run<UndoResponse | RedoResponse>({
      call: (opts) => (isRedo ? redo : undo)(this.c.items, { ...opts, expectedOpId: opId }),
      items: (res) => ({ items: res.items, revision: res.result?.revision ?? 0n }),
    });
    if (!out.ok) {
      if (errorReason(out.err) !== "UNDO_STACK_CHANGED") return this.c.failed(errorText(out.err));
      // Another client's op landed on top: nothing was undone; show what u means now.
      await undoState.reload();
      const now = entryOf();
      return this.c.failed(`Undo stack changed — ${isRedo ? "ctrl-r" : "u"} now: ${now ? singleLine(now.description) : "nothing"}`);
    }
    this.c.toast("info", undoText(out.res, isRedo));
    const ids = this.c.currentIds();
    const back = out.res.items.find((i) => ids.includes(i.id));
    if (back) this.c.selectId(back.id);
  }
}
