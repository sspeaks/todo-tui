// `:view` (design 2.4): new, edit, rename, delete and move saved views, the
// tabs. new and edit open the query prompt, which shows what the text
// parses to (design 4.8) as it is typed: the match count, or the first
// error with its span coloured. enter sends the filter and sort ParseQuery
// gave for exactly that text. A view edit is never an undo entry, so the
// toasts offer no u; delete confirms instead.

import {
  type ParseQueryResponse,
  type View,
  type ViewsClient,
  createClient,
  createView,
  deleteView,
  errorReason,
  moveView,
  parseQuery,
  updateView,
  ViewService,
} from "@todo-tui/client";
import { singleLine, truncate } from "../format/text.js";
import { editLine, insert, type LineState, lineOf } from "../keys/line-editor.js";
import { isChar, type KeyToken } from "../keys/tokenize.js";
import { type Mode, normalMode, type QueryPreview } from "../state/types.js";
import type { Controller } from "./controller.js";
import { Latest } from "./latest.js";
import { errorText } from "./messages.js";

type QueryMode = Extract<Mode, { kind: "query" }>;
type ViewConfirm = Extract<Mode, { kind: "confirm"; action: "delete_view" }>;

/** The subcommands, for completion and the bare `:view`. */
export const VIEW_SUBCOMMANDS = ["new", "edit", "rename", "delete", "move"] as const;

/** How many times an edit rebases silently on a view changed elsewhere before it shows the conflict, as prompts.ts. */
const MAX_REBASES = 3;

const USAGE = ":view new <name> [query] · edit [name] · rename [name] <new> · delete [name] · move [name] <n|left|right|first|last>";

/**
 * Splits `:view` arguments into words; "double quotes" keep spaces in a
 * name (`:view rename "Deep work" Focus`). The rest after the words a
 * subcommand takes is kept as typed, for a query.
 */
export function viewWords(text: string): { words: string[]; rest: (n: number) => string } {
  const words: string[] = [];
  const ends: number[] = [];
  const re = /\s*("(?:[^"\\]|\\.)*"|\S+)/gy;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    const raw = m[1] ?? "";
    words.push(raw.startsWith('"') && raw.endsWith('"') && raw.length >= 2 ? raw.slice(1, -1).replace(/\\(.)/g, "$1") : raw);
    ends.push(re.lastIndex);
  }
  return { words, rest: (n) => (n === 0 ? text : text.slice(ends[n - 1] ?? text.length)).trim() };
}

export class Views {
  private readonly client: ViewsClient;
  private readonly previews: Latest<string, ParseQueryResponse>;

  constructor(private readonly c: Controller) {
    this.client = c.views ?? createClient(ViewService, c.store.transport);
    this.previews = new Latest(
      (text) => parseQuery(this.client, text, { count: true, timeoutMs: c.timing.rpcTimeoutMs }),
      (text, res) => {
        const { mode } = this.c.state;
        if (mode.kind !== "query" || mode.line.text !== text) return;
        this.c.setMode({ ...mode, preview: previewOf(text, res) });
      },
      (text, err) => {
        const { mode } = this.c.state;
        if (mode.kind !== "query" || mode.line.text !== text) return;
        this.c.setMode({ ...mode, preview: { for: text, text: errorText(err), warn: true } });
      },
    );
  }

  /** Resolves once the preview has caught up with the text (tests). */
  previewIdle(): Promise<void> {
    return this.previews.idle();
  }

  /** `:view <args>`. */
  run(args: string): void | Promise<void> {
    const { words, rest } = viewWords(args);
    const [sub, ...more] = words;
    switch (sub) {
      case undefined:
        return this.c.toast("info", USAGE);
      case "new": {
        const name = more[0];
        if (!name) return this.c.toast("info", "Name the view: :view new <name> [query]");
        const query = rest(2);
        if (this.byName(name)) return this.c.failed(`There is a view named “${name}” already`);
        return this.open({ kind: "query", name, line: lineOf(query) }, query !== "");
      }
      case "edit": {
        const view = this.pick(more[0]);
        if (!view) return;
        if (more.length > 1) return this.c.toast("info", "Edit the query in the prompt: :view edit [name]");
        return this.open({ kind: "query", viewId: view.id, name: view.name, revision: view.revision, base: view.query, line: lineOf(view.query) }, false);
      }
      case "rename": {
        // One word renames the current tab; else the view named first, to the rest.
        const [view, name] = more.length >= 2 ? [this.pick(more[0]), more.slice(1).join(" ")] : [this.pick(undefined), more[0]];
        if (!view) return;
        if (!name) return this.c.toast("info", "Name it: :view rename [name] <new>");
        return this.rename(view, name);
      }
      case "delete": {
        const view = this.pick(more[0]);
        if (!view) return;
        const again = view.seed !== undefined ? " It will not come back" : "";
        return this.c.setMode({
          kind: "confirm",
          action: "delete_view",
          viewId: view.id,
          name: view.name,
          text: `Delete view “${truncate(singleLine(view.name), 40)}”?${again} y/N`,
        });
      }
      case "move": {
        const [view, where] = more.length >= 2 ? [this.pick(more[0]), more[1]] : [this.pick(undefined), more[0]];
        if (!view) return;
        const to = this.position(view, where);
        if (to === undefined) return this.c.toast("info", "Where to? :view move [name] <n|left|right|first|last>");
        return this.move(view, to);
      }
      default:
        return this.c.failed(`Not a :view command: ${sub} · ${USAGE}`);
    }
  }

  /** The view named `name` (ignoring case, "." or none for the current tab); else a toast and undefined. */
  private pick(name: string | undefined): View | undefined {
    if (name === undefined || name === ".") {
      const view = this.c.store.getSavedView(this.c.state.tab ?? "");
      if (!view) this.c.toast("info", "This tab is not a saved view; name one");
      return view;
    }
    const view = this.byName(name);
    if (!view) this.c.failed(`No view named “${name}”`);
    return view;
  }

  private byName(name: string): View | undefined {
    const lower = name.trim().toLowerCase();
    return this.c.store.getViews().views.find((v) => v.name.toLowerCase() === lower);
  }

  /** 0-based: "3" is the third tab; left, right, first and last move from where it is. */
  private position(view: View, where: string | undefined): number | undefined {
    const last = this.c.store.getViews().views.length - 1;
    switch (where) {
      case "left":
        return Math.max(0, view.position - 1);
      case "right":
        return Math.min(last, view.position + 1);
      case "first":
        return 0;
      case "last":
        return last;
    }
    const n = Number(where);
    return Number.isInteger(n) && n >= 1 ? Math.min(last, n - 1) : undefined;
  }

  // --- the query prompt ---

  /** Opens the prompt; with `submit` (the query came on the command line), saves it at once. */
  private open(mode: QueryMode, submit: boolean): void | Promise<void> {
    this.c.setMode(mode);
    this.previews.request(mode.line.text);
    if (submit) return this.submit(mode);
  }

  key(mode: QueryMode, key: KeyToken): void | Promise<void> {
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

  paste(mode: QueryMode, text: string): void {
    this.edit(mode, insert(mode.line, singleLine(text)));
  }

  private edit(mode: QueryMode, line: LineState): void {
    this.c.setMode({ ...mode, line, error: undefined });
    this.previews.request(line.text);
  }

  /** The prompt as it is now (a preview may have landed during an await), else as it was. */
  private latest(mode: QueryMode): QueryMode {
    const now = this.c.state.mode;
    return now.kind === "query" && now.name === mode.name && now.viewId === mode.viewId ? now : mode;
  }

  /** Keeps the prompt open with `patch`; what said why the last enter failed goes, unless the patch says it again. */
  private refuse(mode: QueryMode, patch: Partial<QueryMode>): void {
    this.c.setMode({ ...this.latest(mode), error: undefined, ...patch });
    this.c.dropQueued();
  }

  /**
   * enter: ParseQuery for exactly this text (the preview may be for other
   * text), then CreateView, or UpdateView of the filter, sort and count
   * flag with the revision the edit started from.
   */
  private async submit(mode: QueryMode, rebases = 0): Promise<void> {
    const text = mode.line.text;
    if (mode.viewId !== undefined && text === mode.base && mode.conflict === undefined) return this.c.setMode(normalMode);
    let res: ParseQueryResponse;
    try {
      res = await this.c.pipeline.call(({ timeoutMs }) => parseQuery(this.client, text, { ...(timeoutMs !== undefined && { timeoutMs }) }));
    } catch (err) {
      return this.refuse(mode, { error: errorText(err) });
    }
    if (res.errors.length || !res.filter) return this.refuse(mode, { preview: previewOf(text, res) });
    const viewId = mode.viewId;
    if (viewId === undefined) {
      const out = await this.c.pipeline.run({
        call: (opts) => createView(this.client, { name: mode.name, filter: res.filter!, sort: res.sort, showCount: res.showCount }, opts),
        items: noItems,
      });
      if (!out.ok) return this.refuse(mode, { error: this.why(out.err) });
      this.c.setMode(normalMode);
      await this.c.goToView(out.res.view?.id);
      return this.c.toast("info", `Created view “${mode.name}”`);
    }
    const out = await this.c.pipeline.run({
      call: (opts) =>
        updateView(this.client, { id: viewId, filter: res.filter!, sort: res.sort, showCount: res.showCount }, ["filter", "sort", "show_count"], {
          ...opts,
          ...(mode.revision !== undefined && { expectedRevision: mode.revision }),
        }),
      items: noItems,
    });
    if (out.ok) {
      this.c.setMode(normalMode);
      return this.c.toast("info", `Saved view “${mode.name}”`);
    }
    if (errorReason(out.err) !== "REVISION_MISMATCH") return this.refuse(mode, { error: this.why(out.err) });
    // Changed elsewhere, read fresh from the daemon (the stream may not
    // have brought it yet): unchanged query text there means only the name
    // or place moved, so save on the new revision, a few times at most;
    // else show it, and the next enter overwrites.
    let now: View | undefined;
    try {
      const listed = await this.c.pipeline.call(({ timeoutMs }) => this.client.listViews({}, { ...(timeoutMs !== undefined && { timeoutMs }) }));
      now = listed.views.find((v) => v.id === viewId);
    } catch (err) {
      return this.refuse(mode, { error: errorText(err) });
    }
    if (!now) return this.refuse(mode, { error: `“${mode.name}” was deleted elsewhere` });
    if (now.query === mode.base && now.revision !== mode.revision && rebases < MAX_REBASES) {
      return this.submit({ ...this.latest(mode), revision: now.revision }, rebases + 1);
    }
    this.refuse(mode, { revision: now.revision, base: now.query, conflict: now.query });
  }

  /** What an edit's error says; a taken name by name. */
  private why(err: unknown): string {
    if (errorReason(err) === "VIEW_NAME_TAKEN") return "Another view has that name";
    if (errorReason(err) === "VIEW_NOT_FOUND") return "The view was deleted elsewhere";
    return errorText(err);
  }

  // --- rename, move, delete ---

  private async rename(view: View, name: string): Promise<void> {
    const out = await this.c.pipeline.run({ call: (opts) => updateView(this.client, { id: view.id, name }, ["name"], opts), items: noItems });
    if (!out.ok) return this.c.failed(this.why(out.err));
    this.c.toast("info", `Renamed view to “${out.res.view?.name ?? name}”`);
  }

  private async move(view: View, to: number): Promise<void> {
    if (to === view.position) return this.c.toast("info", `“${view.name}” is tab ${to + 1} already`);
    const out = await this.c.pipeline.run({ call: (opts) => moveView(this.client, view.id, to, opts), items: noItems });
    if (!out.ok) return this.c.failed(this.why(out.err));
    this.c.toast("info", `Moved “${view.name}” to tab ${(out.res.view?.position ?? to) + 1}`);
  }

  /** y deletes; any other key cancels, and goes no further. */
  confirmKey(mode: ViewConfirm, key: KeyToken): void | Promise<void> {
    this.c.setMode(normalMode);
    if (key === "y") return this.delete(mode);
  }

  private async delete(mode: ViewConfirm): Promise<void> {
    const out = await this.c.pipeline.run({ call: (opts) => deleteView(this.client, mode.viewId, opts), items: noItems });
    if (!out.ok) return this.c.failed(this.why(out.err));
    this.c.toast("info", `Deleted view “${mode.name}”`);
  }
}

/** A view edit's response carries no Items. */
const noItems = (res: { result?: { revision: bigint } }) => ({ items: [], revision: res.result?.revision ?? 0n });

/** "→ 12 items", or the first error, as `col 14: unknown field "dew"`. */
function previewOf(text: string, res: ParseQueryResponse): QueryPreview {
  const [first] = res.errors;
  if (first) {
    const more = res.errors.length > 1 ? ` · ${res.errors.length - 1} more` : "";
    return { for: text, text: `col ${first.start + 1}: ${singleLine(first.message)}${more}`, warn: true, span: { start: first.start, end: first.end } };
  }
  const n = res.count ?? 0;
  const count = `→ ${n} item${n === 1 ? "" : "s"}`;
  return { for: text, text: res.showCount ? count : `${count} · no tab count`, warn: false };
}
