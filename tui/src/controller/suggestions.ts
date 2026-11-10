// The Suggestions pane (design 2.6), for the sync kinds (ADR-0007): what
// Jira's value won over, and what Jira refused. S opens it, its own keys
// move over the rows, enter ticks a change (on a heading, all of it),
// ctrl-s applies what is ticked in one undoable op, r rejects the
// Suggestion under the cursor. A sync change starts unticked: re-applying
// yours over Jira's is opt-in. A status (its category and resolution kind)
// is one row, as it applies as one. The cursor is the row it is on, by its
// Suggestion and patch: a background change (another client's apply) moves
// the rows, never what r would reject.

import { PatchState, type Suggestion, applySuggestionPatches, rejectSuggestion } from "@todo-tui/client";
import { errorItemId as errorItemOf } from "./bulk.js";
import { normalMode } from "../state/types.js";
import type { Controller } from "./controller.js";
import { errorText, quoted } from "./messages.js";
import { closedWord, statusPatch } from "../format/suggestion.js";

/** One row of the pane: a Suggestion's heading, or one of its changes. */
export type PaneRow =
  | { readonly kind: "heading"; readonly suggestion: Suggestion }
  | {
      readonly kind: "patch";
      readonly suggestion: Suggestion;
      /** The patch ids the row applies: one, or a status's two. */
      readonly ids: readonly string[];
      /** Stale, applied or rejected: it cannot be ticked. */
      readonly closed: boolean;
    };

/** A row's identity: its Suggestion, and for a change its first patch. */
export const rowKey = (r: PaneRow): string => (r.kind === "heading" ? r.suggestion.id : `${r.suggestion.id}/${r.ids[0]}`);

/**
 * The row the cursor is on: the one with its key, else (gone) the one now
 * at its index, kept within the rows; -1 when there are none.
 */
export function cursorIndex(rows: readonly PaneRow[], mode: { readonly cursor: number; readonly at?: string }): number {
  if (!rows.length) return -1;
  const at = mode.at === undefined ? -1 : rows.findIndex((r) => rowKey(r) === mode.at);
  return at >= 0 ? at : Math.max(0, Math.min(mode.cursor, rows.length - 1));
}

/** The pane's rows, in order: each Suggestion's heading, then its changes, a status pair as one. */
export function paneRows(suggestions: readonly Suggestion[]): PaneRow[] {
  const rows: PaneRow[] = [];
  for (const s of suggestions) {
    rows.push({ kind: "heading", suggestion: s });
    const pair = s.patches.filter(statusPatch);
    for (const p of s.patches) {
      if (statusPatch(p) && p !== pair[0]) continue;
      const ids = statusPatch(p) ? pair.map((q) => q.id) : [p.id];
      const closed = (statusPatch(p) ? pair : [p]).some((q) => q.state !== PatchState.PENDING);
      rows.push({ kind: "patch", suggestion: s, ids, closed });
    }
  }
  return rows;
}

export class Suggestions {
  constructor(private readonly c: Controller) {}

  /** The rows as the store holds the Suggestions now. */
  rows(): PaneRow[] {
    return paneRows(this.c.store.suggestions.getSnapshot().suggestions);
  }

  /** S: the pane, ticks cleared, the cursor on the first row; loads what waits. */
  open(): void {
    this.c.setMode({ kind: "suggestions", cursor: 0, ticked: new Set() });
    void this.c.store.suggestions.reload();
  }

  /** The row under the cursor, and its index. */
  private current(mode: Extract<ReturnType<Controller["ui"]["getState"]>["mode"], { kind: "suggestions" }>) {
    const rows = this.rows();
    const index = cursorIndex(rows, mode);
    return { rows, index, row: index < 0 ? undefined : rows[index] };
  }

  /** The pane's mode with the cursor on row `index` of `rows`. */
  private at(mode: Extract<ReturnType<Controller["ui"]["getState"]>["mode"], { kind: "suggestions" }>, rows: readonly PaneRow[], index: number) {
    const row = rows[index];
    return { ...mode, cursor: index, ...(row ? { at: rowKey(row) } : { at: undefined }) };
  }

  /** j/k: the cursor, kept within the rows. */
  move(by: 1 | -1): void {
    const mode = this.c.state.mode;
    if (mode.kind !== "suggestions") return;
    const { rows, index } = this.current(mode);
    if (index < 0) return;
    this.c.setMode(this.at(mode, rows, Math.max(0, Math.min(rows.length - 1, index + by))));
  }

  /** enter: ticks or unticks the change under the cursor; on a heading, all its open changes. */
  toggle(): void {
    const mode = this.c.state.mode;
    if (mode.kind !== "suggestions") return;
    const { rows, row } = this.current(mode);
    if (!row) return;
    const open = (r: PaneRow) => r.kind === "patch" && !r.closed;
    const target = row.kind === "heading" ? rows.filter((r) => open(r) && r.suggestion.id === row.suggestion.id) : open(row) ? [row] : [];
    if (!target.length) {
      if (row.kind === "heading") return this.c.toast("info", "Nothing left to apply in this Suggestion");
      const patch = row.suggestion.patches.find((p) => row.ids.includes(p.id));
      const why = patch ? closedWord(patch) : undefined;
      return this.c.toast("info", why === "changed since" ? "That change cannot be applied: its field changed since" : `That change is ${why ?? "closed"}`);
    }
    const ids = target.flatMap((r) => (r.kind === "patch" ? r.ids : []));
    const ticked = new Set(mode.ticked);
    const all = ids.every((id) => ticked.has(id));
    for (const id of ids) {
      if (all) ticked.delete(id);
      else ticked.add(id);
    }
    this.c.setMode({ ...mode, ticked });
  }

  /**
   * ctrl-s: applies what is ticked, as one op. Someone else's issue asks
   * first ("PLAT-412 is assigned to Alex: change it in Jira? y/N"), as any
   * pushed edit does (the pipeline's NOT_MINE).
   */
  async apply(): Promise<void> {
    const mode = this.c.state.mode;
    if (mode.kind !== "suggestions") return;
    const rows = this.rows();
    // The rows ticked: a status row is one change, with two patch ids.
    const ticked = rows.filter((r): r is Extract<PaneRow, { kind: "patch" }> => r.kind === "patch" && !r.closed && r.ids.some((id) => mode.ticked.has(id)));
    const ids = ticked.flatMap((r) => r.ids);
    if (!ids.length) return this.c.toast("info", "Nothing ticked: enter ticks a change");
    const items = new Set(ticked.map((r) => r.suggestion.itemId));
    // The question names the issue and its assignee: read it if the store lacks it.
    await Promise.all([...items].map((id) => (this.c.store.getItem(id) ? undefined : this.c.store.fetchItem(id).catch(() => undefined))));
    const out = await this.c.pipeline.run({
      call: (opts) => applySuggestionPatches(this.c.store.suggestionsClient, ids, opts),
      items: (res) => ({ items: res.items, revision: res.result?.revision ?? 0n }),
      targets: [...items],
    });
    if (!out.ok) return this.c.failed(errorText(out.err, this.c.store.getItem(errorItemOf(out.err) ?? "") ?? undefined));
    await this.c.store.suggestions.reload();
    const now = this.c.state.mode;
    if (now.kind === "suggestions") {
      const after = this.rows();
      this.c.setMode({ ...this.at(now, after, cursorIndex(after, now)), ticked: new Set() });
    }
    const skippedIds = new Set(out.res.skippedPatchIds);
    const skipped = ticked.filter((r) => r.ids.some((id) => skippedIds.has(id))).length;
    const applied = ticked.length - skipped;
    const note = skipped ? ` · ${skipped} skipped: changed since` : "";
    if (!out.res.items.length) return this.c.toast("info", `Nothing applied${note}`);
    this.c.toastOp(out.res.result?.opId, `applied ${applied} change${applied === 1 ? "" : "s"}`, note);
  }

  /** r: rejects the Suggestion under the cursor; Jira's value stays. */
  async reject(): Promise<void> {
    const mode = this.c.state.mode;
    if (mode.kind !== "suggestions") return;
    const { row } = this.current(mode);
    if (!row) return;
    const s = row.suggestion;
    const out = await this.c.pipeline.run({
      call: (opts) => rejectSuggestion(this.c.store.suggestionsClient, s.id, opts),
      items: () => ({ items: [], revision: 0n }),
    });
    if (!out.ok) return this.c.failed(errorText(out.err));
    await this.c.store.suggestions.reload();
    const now = this.c.state.mode;
    if (now.kind === "suggestions") {
      const ticked = new Set([...now.ticked].filter((id) => !s.patches.some((p) => p.id === id)));
      const after = this.rows();
      this.c.setMode({ ...this.at(now, after, cursorIndex(after, now)), ticked });
    }
    const item = this.c.store.getItem(s.itemId);
    this.c.toast("info", `Rejected: ${item ? quoted(item.title) : "the Suggestion"} keeps Jira's value`);
  }

  /** :w: what the pane has ticked, from its command line. */
  applyFromCommand(): void | Promise<void> {
    if (this.c.state.mode.kind !== "suggestions") return this.c.toast("info", ":w applies what the Suggestions pane has ticked: S opens it");
    return this.apply();
  }

  /** esc, q, S: back to the list. */
  close(): void {
    this.c.setMode(normalMode);
  }
}
