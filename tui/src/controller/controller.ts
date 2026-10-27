// The dispatcher (Decision 2): every key goes through the input queue to
// the mode that owns it, and becomes a change to the UiStore or an action
// through the pipeline. It also keeps the current tab's view observed and
// its selection pinned, and runs the midnight poll. No Ink here, so the
// controller tests drive it directly.

import type { Item, ItemsClient, LiveStore, LiveView, ViewSnapshot } from "@todo-tui/client";
import type { Key } from "ink";
import { dayOf, localDay } from "../format/dates.js";
import { singleLine } from "../format/text.js";
import { completeCommand, resolveCommand } from "../keys/commands.js";
import { helpLines } from "../keys/help.js";
import { type Action, BINDINGS, type Binding, isAvailable, type KeymapMode } from "../keys/keymap.js";
import { editLine, emptyLine, insert, type LineState, lineOf } from "../keys/line-editor.js";
import { isChar, type KeyToken, keyLabel, parseKeys, tokenize } from "../keys/tokenize.js";
import { buildTrie, continuations, step, type TrieNode } from "../keys/trie.js";
import { layout } from "../state/layout.js";
import { halfPage, initialList, type ListState, listOf, move, reconcile, type Resolved, sameList, select } from "../state/selection.js";
import { type Mode, normalMode, type TabId, type Target, type UiState } from "../state/types.js";
import { UiStore } from "../state/ui-store.js";
import { ItemActions } from "./actions.js";
import { DEFAULT_TIMING, Pipeline, type Timing } from "./pipeline.js";
import { Prompts } from "./prompts.js";
import { QuickAdd, typedLines } from "./quick-add.js";
import { type Input, InputQueue } from "./queue.js";

export const INFO_TOAST_MS = 4_000;
export const ERROR_TOAST_MS = 8_000;
/** How often the midnight poll looks at the date: a long timer would stall while a laptop sleeps. */
export const MIDNIGHT_POLL_MS = 60_000;

export interface ControllerOptions {
  store: LiveStore;
  /** The ItemService client actions call. */
  items: ItemsClient;
  ui?: UiStore;
  /** The clock the midnight poll compares with the daemon's today (default: the system clock). */
  now?: () => Date;
  /** Delays and deadlines (tests shorten them). */
  timing?: Partial<Timing>;
}

const clamp = (n: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, n));

const describe = (err: unknown) => (err instanceof Error ? err.message : String(err));

export class Controller {
  readonly store: LiveStore;
  readonly items: ItemsClient;
  readonly ui: UiStore;
  readonly timing: Timing;
  /** Runs actions: the RPC, the response into the store, the reload. */
  readonly pipeline: Pipeline;
  /** Quits the app (Ink's exit); set by the shell. */
  onQuit: (() => void) | undefined;
  private readonly actions: ItemActions;
  private readonly prompts: Prompts;
  private readonly quickAdd: QuickAdd;
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
    this.items = opts.items;
    this.ui = opts.ui ?? new UiStore();
    this.now = opts.now ?? (() => new Date());
    this.timing = { ...DEFAULT_TIMING, ...opts.timing };
    this.tries = { normal: buildTrie(BINDINGS, "normal"), help: buildTrie(BINDINGS, "help") };
    this.queue = new InputQueue(
      (input) => this.handle(input),
      (err) => this.toast("error", describe(err)),
    );
    this.pipeline = new Pipeline(
      this.store,
      this.timing,
      () => this.currentView(),
      (busy) => this.ui.setState((s) => (s.busy === busy ? s : { ...s, busy })),
    );
    this.actions = new ItemActions(this);
    this.prompts = new Prompts(this);
    this.quickAdd = new QuickAdd(this);
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
    this.push(tokenize(input, key));
  }

  /** One usePaste event. */
  paste(text: string): void {
    this.push([{ paste: text }]);
  }

  /** Feeds keys written as parseKeys reads them ("jj<esc>"), and resolves once they are handled. */
  press(keys: string): Promise<void> {
    this.push(parseKeys(keys));
    return this.queue.idle();
  }

  /** Resolves once every input so far is handled. */
  idle(): Promise<void> {
    return this.queue.idle();
  }

  /** Resolves once the quick-add and prompt previews have caught up with the text (tests). */
  async previewIdle(): Promise<void> {
    await Promise.all([this.quickAdd.previewIdle(), this.prompts.previewIdle()]);
  }

  /**
   * Queues input behind the action in flight, if any. Only q or ctrl-c in
   * normal mode, with nothing else waiting, goes past it: quitting waits
   * for the action at most a second.
   */
  private push(inputs: readonly Input[]): void {
    const { mode } = this.state;
    const [first] = inputs;
    const quitKey = first === "q" || first === "ctrl+c";
    if (quitKey && this.queue.inFlight && !this.queue.waiting && mode.kind === "normal" && !mode.pending.length) {
      void this.quit();
      return;
    }
    this.queue.push(inputs);
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

  /** The selected Item in the current tab, as the store holds it. */
  selectedItem(): Item | undefined {
    const id = this.selectedId();
    return id === undefined ? undefined : this.store.getItem(id);
  }

  /** What an action started now acts on: the selected Item, with the revision rendered (marks come with phase 3). */
  target(): Target | undefined {
    const item = this.selectedItem();
    return item && { ids: [item.id], bulk: false, revisions: new Map([[item.id, item.revision]]) };
  }

  /** The current tab's ids, as last loaded. */
  currentIds(): readonly string[] {
    return this.current()?.ids ?? [];
  }

  /** The current tab's live view. */
  currentView(): LiveView | undefined {
    const { tab } = this.state;
    return tab === undefined ? undefined : this.store.view({ viewId: tab });
  }

  /** Selects `id` in the current tab, scrolled into view, if the tab lists it. */
  selectId(id: string): void {
    const cur = this.current();
    const at = cur?.ids.indexOf(id) ?? -1;
    if (!cur || at < 0) return;
    this.setList(cur.tab, listOf(select(this.listState(cur.tab), cur.ids, this.height, at)));
  }

  /** The daemon's today as a day number (the local date until the counts load). */
  today(): number {
    const today = this.store.getCounts().today;
    return today ? dayOf(today) : localDay(this.now());
  }

  /** The current mode, for the modes' own handlers. */
  get state(): UiState {
    return this.ui.getState();
  }

  setMode(mode: Mode): void {
    this.ui.setState((s) => ({ ...s, mode }));
  }

  /**
   * After an action failed: the keys queued behind it are dropped, since
   * they were meant for the state a success would have left; the error
   * toast says how many. In quick-add, text typed behind a failed capture
   * is kept as lines to capture next (Decision 6: typed text is never lost).
   */
  failed(text: string): void {
    const queued = this.queue.take();
    let dropped = queued.length;
    const { mode } = this.state;
    if (mode.kind === "quickAdd" && queued.length) {
      const typed = typedLines(queued);
      dropped = typed.dropped;
      if (typed.lines.length) this.setMode({ ...mode, rest: [...(mode.rest ?? []), ...typed.lines] });
    }
    this.toast("error", dropped ? `${text} · ${dropped} key${dropped === 1 ? "" : "s"} dropped` : text);
  }

  /**
   * A typed character and the ones queued right behind it, as one run of
   * text: a text mode inserts a burst (or a paste without bracketed
   * paste) at once, with one preview request.
   */
  charRun(first: KeyToken): string {
    const run = [first, ...this.queue.takeWhile((i) => typeof i === "string" && isChar(i))];
    return run.map((k) => (k === "space" ? " " : k)).join("");
  }

  /** The keys queued behind an action that kept its mode open (a conflict): dropped, with a notice when there were any. */
  dropQueued(): void {
    const dropped = this.queue.take().length;
    if (dropped) this.toast("info", `${dropped} key${dropped === 1 ? "" : "s"} dropped`);
  }

  /**
   * An action's toast (design 2.8): the undo entry's description, when the
   * top of the undo stack is this action's op, and how to undo it. When
   * another op landed on top first, `fallback` says what was done, and u
   * is not offered: it would undo that other op.
   */
  toastOp(opId: string | undefined, fallback: string): void {
    const top = this.store.undoState.getSnapshot().undo;
    this.toast("info", top && opId !== undefined && top.opId === opId ? `${singleLine(top.description)} · u undo` : fallback);
  }

  /** A plain edit's toast: only when the Item left the current tab. */
  toastIfLeft(id: string, opId: string | undefined, fallback: string): void {
    if (!this.currentIds().includes(id)) this.toastOp(opId, fallback);
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

  /** Quits; with an action in flight, once it settles or a second has passed. */
  async quit(): Promise<void> {
    if (this.state.quitting) return;
    this.ui.setState((s) => ({ ...s, quitting: true }));
    if (this.queue.inFlight) {
      let timer: ReturnType<typeof setTimeout> | undefined;
      await Promise.race([this.queue.settled(), new Promise<void>((resolve) => (timer = setTimeout(resolve, this.timing.quitMs)))]);
      clearTimeout(timer);
    }
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

  /** The list's height, without the rows the mode takes. */
  private get height(): number {
    const { size, mode } = this.state;
    return layout(size.columns, size.rows, mode.kind).bodyHeight;
  }

  private listState(tab: TabId): ListState {
    return this.state.lists.get(tab) ?? initialList;
  }

  private setList(tab: TabId, list: ListState): void {
    this.ui.setState((s) => ({ ...s, lists: new Map(s.lists).set(tab, list) }));
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
      case "quickAdd":
        return this.quickAdd.key(mode, input);
      case "prompt":
        return this.prompts.key(mode, input);
      case "picker":
        return this.actions.pickerKey(mode, input);
      case "confirm":
        return this.actions.confirmKey(mode, input);
      default:
        // Search arrives in phase 3 of M4.
        if (input === "esc" || input === "ctrl+c") this.setMode(normalMode);
    }
  }

  private normalKey(pending: readonly KeyToken[], key: KeyToken): void | Promise<void> {
    if (pending.length && (key === "esc" || key === "ctrl+c")) return this.setMode(normalMode);
    const r = step(this.tries.normal, pending, key);
    if (r.kind === "pending") return this.setMode({ kind: "normal", pending: r.pending });
    if (pending.length) this.setMode(normalMode);
    if (r.kind === "unbound") return this.toast("info", `${r.keys.map(keyLabel).join(" ")}: not bound · ? for help`);
    return this.runBinding(r.binding);
  }

  /** Help's keys, through its own trie: a prefix (g) waits for the next key, and esc drops it. */
  private helpKey(mode: Extract<Mode, { kind: "help" }>, key: KeyToken): void | Promise<void> {
    const pending = mode.pending ?? [];
    const settled: Mode = { kind: "help", scroll: mode.scroll };
    if (pending.length && (key === "esc" || key === "ctrl+c")) return this.setMode(settled);
    const r = step(this.tries.help, pending, key);
    if (r.kind === "pending") return this.setMode({ ...settled, pending: r.pending });
    if (pending.length) this.setMode(settled);
    if (r.kind === "run") return this.runBinding(r.binding);
  }

  private commandKey(line: LineState, key: KeyToken): void | Promise<void> {
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
    const next = isChar(key) ? insert(line, this.charRun(key)) : editLine(line, key);
    if (next) this.setMode({ kind: "command", line: next });
  }

  private onPaste(mode: Mode, text: string): void | Promise<void> {
    switch (mode.kind) {
      case "command":
        return this.setMode({ kind: "command", line: insert(mode.line, singleLine(text)) });
      case "quickAdd":
        return this.quickAdd.paste(mode, text);
      case "prompt":
        return this.prompts.paste(mode, text);
      case "confirm":
        // Anything but y cancels.
        return this.setMode(normalMode);
      case "normal":
        // Never keys: a pasted "xxdd" must not run.
        return this.toast("info", "Paste ignored: press a to add");
    }
  }

  private runCommand(text: string): void | Promise<void> {
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

  private runBinding(b: Binding): void | Promise<void> {
    if (!isAvailable(b)) return this.toast("info", `${b.help}: not available yet`);
    return this.run(b.action);
  }

  private run(action: Action): void | Promise<void> {
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
      case "quick_add":
        return action.child ? this.toast("info", "Not available yet") : this.quickAdd.open();
      case "prompt":
        return this.prompts.open(action.prompt);
      case "status_picker":
        return this.actions.openPicker();
      case "set_status":
        return this.actions.setStatus(action.status);
      case "set_priority":
        return this.actions.setPriority(action.priority);
      case "clear_waiting":
        return this.actions.clearWaiting();
      case "archive_toggle":
        return this.actions.toggleArchive();
      case "delete":
        return this.actions.confirmDelete();
      case "undo":
        return this.actions.undo(false);
      case "redo":
        return this.actions.undo(true);
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
