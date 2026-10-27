// The dispatcher (Decision 2): every key goes through the input queue to
// the mode that owns it, and becomes a change to the UiStore (or, from
// phase 2, an RPC). It also keeps the current tab's view observed and its
// selection pinned, and runs the midnight poll. No Ink here, so the
// controller tests drive it directly.

import type { LiveStore, ViewSnapshot } from "@todo-tui/client";
import type { Key } from "ink";
import { dayOf, localDay } from "../format/dates.js";
import { singleLine } from "../format/text.js";
import { completeCommand, resolveCommand } from "../keys/commands.js";
import { helpLines } from "../keys/help.js";
import { type Action, BINDINGS, type Binding, isAvailable, type KeymapMode } from "../keys/keymap.js";
import { editLine, emptyLine, insert, type LineState, lineOf } from "../keys/line-editor.js";
import { type KeyToken, keyLabel, parseKeys, tokenize } from "../keys/tokenize.js";
import { buildTrie, continuations, step, type TrieNode } from "../keys/trie.js";
import { layout } from "../state/layout.js";
import { halfPage, initialList, type ListState, listOf, move, reconcile, type Resolved, sameList, select } from "../state/selection.js";
import { type Mode, normalMode, type TabId, type UiState } from "../state/types.js";
import { UiStore } from "../state/ui-store.js";
import { type Input, InputQueue } from "./queue.js";

export const INFO_TOAST_MS = 4_000;
export const ERROR_TOAST_MS = 8_000;
/** How often the midnight poll looks at the date: a long timer would stall while a laptop sleeps. */
export const MIDNIGHT_POLL_MS = 60_000;

export interface ControllerOptions {
  store: LiveStore;
  ui?: UiStore;
  /** The clock the midnight poll compares with the daemon's today (default: the system clock). */
  now?: () => Date;
}

const clamp = (n: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, n));

const describe = (err: unknown) => (err instanceof Error ? err.message : String(err));

export class Controller {
  readonly store: LiveStore;
  readonly ui: UiStore;
  /** Quits the app (Ink's exit); set by the shell. */
  onQuit: (() => void) | undefined;
  private readonly now: () => Date;
  private readonly queue: InputQueue;
  private readonly tries: Readonly<Record<KeymapMode, TrieNode>>;
  private readonly stops: (() => void)[] = [];
  /** The current tab's view, observed so that it stays live (only that one). */
  private watched: { tab: TabId; stop: () => void } | undefined;
  private toastTimer: ReturnType<typeof setTimeout> | undefined;
  private toastSeq = 0;
  /** The daemon's today the midnight poll last refreshed for. */
  private refreshedFor: number | undefined;
  private idsCache: { snap: ViewSnapshot; ids: readonly string[] } | undefined;

  constructor(opts: ControllerOptions) {
    this.store = opts.store;
    this.ui = opts.ui ?? new UiStore();
    this.now = opts.now ?? (() => new Date());
    this.tries = { normal: buildTrie(BINDINGS, "normal"), help: buildTrie(BINDINGS, "help") };
    this.queue = new InputQueue(
      (input) => this.handle(input),
      (err) => this.toast("error", describe(err)),
    );
  }

  /** Observes the store: the tabs, the current view, the counts and the undo state; starts the midnight poll. */
  start(): void {
    if (this.stops.length) return;
    this.stops.push(this.store.subscribe(() => this.sync()));
    // The tab badges and the daemon's today, and the undo hint.
    this.stops.push(this.store.subscribeCounts(() => {}));
    this.stops.push(this.store.undoState.subscribe(() => {}));
    const poll = setInterval(() => this.midnight(), MIDNIGHT_POLL_MS);
    this.stops.push(() => clearInterval(poll));
    this.sync();
  }

  stop(): void {
    for (const stop of this.stops.splice(0)) stop();
    this.watched?.stop();
    this.watched = undefined;
    if (this.toastTimer) clearTimeout(this.toastTimer);
    this.toastTimer = undefined;
  }

  // --- input ---

  /** One useInput event. */
  input(input: string, key: Partial<Key>): void {
    this.queue.push(tokenize(input, key));
  }

  /** One usePaste event. */
  paste(text: string): void {
    this.queue.push([{ paste: text }]);
  }

  /** Feeds keys written as parseKeys reads them ("jj<esc>"), and resolves once they are handled. */
  press(keys: string): Promise<void> {
    this.queue.push(parseKeys(keys));
    return this.queue.idle();
  }

  /** Resolves once every input so far is handled. */
  idle(): Promise<void> {
    return this.queue.idle();
  }

  /** The terminal size; crossing the split breakpoint closes the overlay and gives the list focus. */
  setSize(columns: number, rows: number): void {
    const { size } = this.state;
    if (size.columns === columns && size.rows === rows) return;
    const crossed = layout(size.columns, size.rows).wide !== layout(columns, rows).wide;
    this.ui.setState((s) => ({ ...s, size: { columns, rows }, ...(crossed && { overlay: { kind: "none" }, focus: "list" }) }));
    this.sync();
  }

  /** The detail pane's content height for the Item it shows, so that scrolling stops at its end. */
  setDetailLines(id: string | undefined, lines: number): void {
    this.ui.setState((s) =>
      s.detail.id === id && s.detail.lines === lines ? s : { ...s, detail: { id, scroll: s.detail.id === id ? s.detail.scroll : 0, lines } },
    );
  }

  /** The selected Item's id in the current tab. */
  selectedId(): string | undefined {
    const cur = this.current();
    return cur && reconcile(this.listState(cur.tab), cur.ids, this.height).selectedId;
  }

  /** The which-key hint for a pending prefix: "g… g First row". */
  whichKey(pending: readonly KeyToken[]): string {
    const groups = new Map<string, string[]>();
    for (const [key, b] of continuations(this.tries.normal, pending)) groups.set(b.help, [...(groups.get(b.help) ?? []), keyLabel(key)]);
    const hints = [...groups].map(([help, keys]) => `${keys.length > 2 ? `${keys[0]}–${keys.at(-1)}` : keys.join(" ")} ${help}`);
    return `${pending.map(keyLabel).join(" ")}… ${hints.join(" · ")}`;
  }

  /** Shows a toast on the message line: info for 4 s, an error for 8 s or until the next key. */
  toast(level: "info" | "error", text: string): void {
    const id = ++this.toastSeq;
    this.ui.setState((s) => ({ ...s, toast: { id, level, text } }));
    if (this.toastTimer) clearTimeout(this.toastTimer);
    this.toastTimer = setTimeout(
      () => {
        this.toastTimer = undefined;
        this.ui.setState((s) => (s.toast?.id === id ? { ...s, toast: undefined } : s));
      },
      level === "error" ? ERROR_TOAST_MS : INFO_TOAST_MS,
    );
  }

  quit(): void {
    if (this.state.quitting) return;
    this.ui.setState((s) => ({ ...s, quitting: true }));
    this.onQuit?.();
  }

  /**
   * The midnight poll: once the local date passes the daemon's today,
   * re-read the views and the counts, once per today (design 4.4). The new
   * today then moves Today's rows and the relative date labels.
   */
  midnight(): void {
    const today = this.store.getCounts().today;
    if (!today) return;
    const day = dayOf(today);
    if (localDay(this.now()) <= day || this.refreshedFor === day) return;
    this.refreshedFor = day;
    this.store.refresh();
  }

  // --- state ---

  private get state(): UiState {
    return this.ui.getState();
  }

  private get height(): number {
    const { columns, rows } = this.state.size;
    return layout(columns, rows).bodyHeight;
  }

  private listState(tab: TabId): ListState {
    return this.state.lists.get(tab) ?? initialList;
  }

  private setList(tab: TabId, list: ListState): void {
    this.ui.setState((s) => ({ ...s, lists: new Map(s.lists).set(tab, list) }));
  }

  private setMode(mode: Mode): void {
    this.ui.setState((s) => ({ ...s, mode }));
  }

  /** The current tab and its rows. */
  private current(): { tab: TabId; snap: ViewSnapshot; ids: readonly string[] } | undefined {
    const { tab } = this.state;
    if (tab === undefined) return undefined;
    const snap = this.store.view({ viewId: tab }).getSnapshot();
    if (this.idsCache?.snap !== snap) this.idsCache = { snap, ids: snap.items.map((i) => i.id) };
    return { tab, snap, ids: this.idsCache.ids };
  }

  /**
   * After any store change: keeps the tab one that exists, observes its
   * view, and pins its selection, so that a later change starts from where
   * this one left it and background changes never move the selection.
   */
  private sync(): void {
    const { views } = this.store.getViews();
    const { tab } = this.state;
    const first = views[0];
    if (first && (tab === undefined || !views.some((v) => v.id === tab))) this.ui.setState((s) => ({ ...s, tab: first.id }));
    const current = this.state.tab;
    if (this.watched?.tab !== current) {
      this.watched?.stop();
      this.watched = current === undefined ? undefined : { tab: current, stop: this.store.view({ viewId: current }).subscribe(() => {}) };
    }
    const cur = this.current();
    // A view still loading (after a tab switch, say) has no rows yet; its selection waits for them.
    if (!cur || cur.snap.status !== "ready") return;
    const list = this.listState(cur.tab);
    const next = listOf(reconcile(list, cur.ids, this.height));
    if (!sameList(list, next)) this.setList(cur.tab, next);
  }

  // --- keys ---

  private handle(input: Input): void | Promise<void> {
    if (this.state.quitting) return;
    if (this.state.toast?.level === "error") this.clearToast();
    const { mode } = this.state;
    if (typeof input !== "string") return this.onPaste(mode, input.paste);
    switch (mode.kind) {
      case "normal":
        return this.normalKey(mode.pending, input);
      case "help":
        return this.helpKey(mode, input);
      case "command":
        return this.commandKey(mode.line, input);
      default:
        // The other modes arrive in phases 2 and 3 of M4.
        if (input === "esc" || input === "ctrl+c") this.setMode(normalMode);
    }
  }

  private normalKey(pending: readonly KeyToken[], key: KeyToken): void {
    if (pending.length && (key === "esc" || key === "ctrl+c")) return this.setMode(normalMode);
    const r = step(this.tries.normal, pending, key);
    if (r.kind === "pending") return this.setMode({ kind: "normal", pending: r.pending });
    if (pending.length) this.setMode(normalMode);
    if (r.kind === "unbound") return this.toast("info", `${r.keys.map(keyLabel).join(" ")}: not bound · ? for help`);
    return this.runBinding(r.binding);
  }

  /** Help's keys, through its own trie: a prefix (g) waits for the next key, and esc drops it. */
  private helpKey(mode: Extract<Mode, { kind: "help" }>, key: KeyToken): void {
    const pending = mode.pending ?? [];
    const settled: Mode = { kind: "help", scroll: mode.scroll };
    if (pending.length && (key === "esc" || key === "ctrl+c")) return this.setMode(settled);
    const r = step(this.tries.help, pending, key);
    if (r.kind === "pending") return this.setMode({ ...settled, pending: r.pending });
    if (pending.length) this.setMode(settled);
    if (r.kind === "run") this.runBinding(r.binding);
  }

  private commandKey(line: LineState, key: KeyToken): void {
    switch (key) {
      case "esc":
      case "ctrl+c":
        return this.setMode(normalMode);
      case "enter":
        this.setMode(normalMode);
        return this.runCommand(line.text);
      case "tab":
        return this.setMode({ kind: "command", line: lineOf(completeCommand(line.text)) });
      case "backspace":
        // As in vim, backspace on an empty command line leaves it.
        if (line.text === "") return this.setMode(normalMode);
    }
    const next = editLine(line, key);
    if (next) this.setMode({ kind: "command", line: next });
  }

  private onPaste(mode: Mode, text: string): void {
    if (mode.kind === "command") this.setMode({ kind: "command", line: insert(mode.line, singleLine(text)) });
    else if (mode.kind === "normal") this.toast("info", "Paste ignored: no text field is open");
  }

  private runCommand(text: string): void {
    const r = resolveCommand(text);
    if (r.kind === "message") return this.toast(r.level, r.text);
    if (r.kind === "none") return;
    switch (r.command) {
      case "quit":
        return this.quit();
      case "help":
        return this.setMode({ kind: "help", scroll: 0 });
      case "refresh":
        this.store.refresh();
        void this.store.undoState.reload();
        return this.toast("info", "Refreshed");
    }
  }

  private runBinding(b: Binding): void {
    if (!isAvailable(b)) return this.toast("info", `${b.help}: not available yet`);
    this.run(b.action);
  }

  private run(action: Action): void {
    const h = this.height;
    const half = Math.max(1, Math.floor(h / 2));
    switch (action.type) {
      case "move":
        return this.navigate((s) => s + action.by, (l, ids) => move(l, ids, h, action.by));
      case "top":
        return this.navigate(() => 0, (l, ids) => select(l, ids, h, 0));
      case "bottom":
        return this.navigate((_, max) => max, (l, ids) => select(l, ids, h, ids.length - 1));
      case "half_page":
        return this.navigate((s) => s + half * action.dir, (l, ids) => halfPage(l, ids, h, action.dir));
      case "view":
        return this.switchTab(this.store.getViews().views[action.position - 1]?.id);
      case "view_step": {
        const { views } = this.store.getViews();
        const at = views.findIndex((v) => v.id === this.state.tab);
        return this.switchTab(views[(at + action.by + views.length) % views.length]?.id);
      }
      case "focus_toggle":
        if (layout(this.state.size.columns, this.state.size.rows).wide) {
          return this.ui.setState((s) => ({ ...s, focus: s.focus === "list" ? "detail" : "list" }));
        }
        return this.state.overlay.kind === "detail" ? this.closeOverlay() : this.openDetail();
      case "open_detail":
        return this.openDetail();
      case "back":
        if (this.state.overlay.kind === "detail" || this.state.focus === "detail") this.closeOverlay();
        return;
      case "help":
        return this.setMode({ kind: "help", scroll: 0 });
      case "help_close":
        return this.setMode(normalMode);
      case "command":
        return this.setMode({ kind: "command", line: emptyLine });
      case "quit":
        return this.quit();
      default:
        return this.toast("info", "Not available yet");
    }
  }

  /**
   * A navigation key: scrolls the help, or the detail pane when it has
   * focus (`scroll` maps the offset and the largest one to the new
   * offset), or else moves the list's selection.
   */
  private navigate(scroll: (offset: number, max: number) => number, list: (l: ListState, ids: readonly string[]) => Resolved): void {
    const { mode } = this.state;
    if (mode.kind === "help") {
      const max = Math.max(0, helpLines().length - this.height);
      return this.setMode({ kind: "help", scroll: clamp(scroll(mode.scroll, max), 0, max) });
    }
    if (this.state.focus === "detail") {
      const id = this.selectedId();
      return this.ui.setState((s) => {
        const max = Math.max(0, s.detail.lines - this.height);
        const from = s.detail.id === id ? s.detail.scroll : 0;
        return { ...s, detail: { ...s.detail, id, scroll: clamp(scroll(from, max), 0, max) } };
      });
    }
    const cur = this.current();
    if (!cur || cur.snap.status !== "ready") return;
    this.setList(cur.tab, listOf(list(this.listState(cur.tab), cur.ids)));
  }

  /** Focuses the detail pane: beside the list when wide, else as the full-screen overlay. */
  private openDetail(): void {
    if (this.selectedId() === undefined) return;
    const { columns, rows } = this.state.size;
    const wide = layout(columns, rows).wide;
    this.ui.setState((s) => ({ ...s, focus: "detail", overlay: wide ? s.overlay : { kind: "detail" } }));
  }

  private closeOverlay(): void {
    this.ui.setState((s) => ({ ...s, focus: "list", overlay: { kind: "none" } }));
  }

  private switchTab(tab: TabId | undefined): void {
    if (tab === undefined || tab === this.state.tab) return;
    this.ui.setState((s) => ({ ...s, tab, focus: "list", overlay: { kind: "none" } }));
    this.sync();
  }

  private clearToast(): void {
    if (this.toastTimer) clearTimeout(this.toastTimer);
    this.toastTimer = undefined;
    this.ui.setState((s) => ({ ...s, toast: undefined }));
  }
}
