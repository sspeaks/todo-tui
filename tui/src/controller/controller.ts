// The dispatcher (Decision 2): every key goes through the input queue to
// the mode that owns it, and becomes a change to the UiStore or an action
// through the pipeline. It also keeps the current tab's view observed and
// its selection pinned, and runs the midnight poll. No Ink here, so the
// controller tests drive it directly.

import { type Item, type ItemsClient, type LiveStore, type LiveView, type ViewSnapshot, envFromProcess, resolvePaths } from "@todo-tui/client";
import type { Key } from "ink";
import { dayOf, localDay } from "../format/dates.js";
import { singleLine } from "../format/text.js";
import { completeCommand, resolveCommand } from "../keys/commands.js";
import { helpLines } from "../keys/help.js";
import { type Action, BINDINGS, type Binding, isAvailable, type KeymapMode } from "../keys/keymap.js";
import { altInLine, editLine, emptyLine, insert, type LineState, lineOf } from "../keys/line-editor.js";
import { altKey, isChar, type KeyToken, keyLabel, parseKeys, tokenize } from "../keys/tokenize.js";
import { buildTrie, continuations, step, type TrieNode } from "../keys/trie.js";
import { layout } from "../state/layout.js";
import { anchorIndex, markedInOrder, pruneMarks, rangeMarks, toggleMark } from "../state/marks.js";
import { halfPage, initialList, type ListState, listOf, move, reconcile, type Resolved, sameList, select } from "../state/selection.js";
import { matchText, searchQuery, searchTab, tabQuery } from "../state/tabs.js";
import { type Mode, noMarks, normalMode, type TabId, type Target, type UiState } from "../state/types.js";
import { UiStore } from "../state/ui-store.js";
import { ItemActions } from "./actions.js";
import { MAX_BULK, TOO_MANY } from "./bulk.js";
import { DescriptionEditor, type EditorRunner, type Suspend, systemEditor } from "./editor.js";
import { Narrowing } from "./narrow.js";
import { stopSelf } from "./job.js";
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
  /** E's editor (default: $VISUAL / $EDITOR through the shell's suspendTerminal). */
  editor?: EditorRunner;
  /** Where E keeps a description it could not save, under recovered/ (default: the state dir, paths.ts). */
  stateDir?: string;
  /** ctrl-z: stops the process as a shell job, resolving once it continues; false when nothing stopped it (default: job.ts). */
  stopProcess?: () => Promise<boolean>;
}

/** How a toast is cut when it does not fit (Toast.keepEnd). */
export interface ToastOptions {
  readonly keepEnd?: boolean;
}

/** The actions a key queued behind a failed action still runs: they only move the selection or the tab. */
const NAVIGATION: ReadonlySet<Action["type"]> = new Set(["move", "top", "bottom", "half_page", "view", "view_step"]);

/** The modes that edit a line of text, where alt+X can be a word edit. */
const TEXT_MODES: ReadonlySet<Mode["kind"]> = new Set(["quickAdd", "prompt", "command", "search"]);

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
  /** Hands the terminal over and back (Ink's suspendTerminal), for E and ctrl-z; set by the shell. */
  suspendTerminal: Suspend | undefined;
  private readonly actions: ItemActions;
  private readonly prompts: Prompts;
  private readonly quickAdd: QuickAdd;
  private readonly narrowing: Narrowing;
  private readonly editor: DescriptionEditor;
  private readonly stopProcess: () => Promise<boolean>;
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
  private readonly idsCache = new WeakMap<ViewSnapshot, readonly string[]>();
  private readonly idSets = new WeakMap<ViewSnapshot, ReadonlySet<string>>();

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
      () => this.currentViews(),
      (busy) => this.ui.setState((s) => (s.busy === busy ? s : { ...s, busy })),
    );
    this.actions = new ItemActions(this);
    this.prompts = new Prompts(this);
    this.quickAdd = new QuickAdd(this);
    this.narrowing = new Narrowing(this);
    const stateDir = opts.stateDir;
    this.editor = new DescriptionEditor(
      this,
      opts.editor ?? systemEditor(() => this.suspendTerminal),
      () => stateDir ?? resolvePaths(envFromProcess()).logs,
    );
    this.stopProcess = opts.stopProcess ?? (() => stopSelf());
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
    this.narrowing.release();
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

  /** Resolves once the quick-add and prompt previews, and `/`'s rows, have caught up with the text (tests). */
  async previewIdle(): Promise<void> {
    await Promise.all([this.quickAdd.previewIdle(), this.prompts.previewIdle(), this.narrowing.idle()]);
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

  /**
   * What an action started now acts on, with the revisions rendered: for a
   * mark-aware key in a tab with marks, every marked Item in list order
   * (Decision 8), else the selected Item. More than a BatchMutate takes is
   * refused, with a toast, and nothing acts.
   */
  target(markAware = false): Target | undefined {
    const marked = markAware ? this.markedIds() : [];
    if (marked.length > MAX_BULK) {
      this.toast("error", TOO_MANY);
      return undefined;
    }
    const ids = marked.length ? marked : [this.selectedId()].filter((id) => id !== undefined);
    const items = ids.flatMap((id) => this.store.getItem(id) ?? []);
    if (!items.length) return undefined;
    return { ids: items.map((i) => i.id), bulk: marked.length > 0, revisions: new Map(items.map((i) => [i.id, i.revision])) };
  }

  /** The marked ids still in the tab's own rows, in their order. */
  markedIds(): string[] {
    return markedInOrder(this.state.marks, this.base()?.ids ?? []);
  }

  /** The current tab's ids as listed (narrowed, when `/` narrows), as last loaded. */
  currentIds(): readonly string[] {
    return this.current()?.ids ?? [];
  }

  /** The views an action's settle reloads: the rows shown, and the tab's own (whose ids the marks follow). */
  currentViews(): LiveView[] {
    const { tab, narrow } = this.state;
    if (tab === undefined) return [];
    const base = this.store.view(tabQuery(tab));
    return narrow?.shown === undefined ? [base] : [this.store.view(tabQuery(tab, narrow.shown)), base];
  }

  /** Selects `id` in the current tab, scrolled into view, if the tab lists it. */
  selectId(id: string): void {
    const cur = this.current();
    const at = cur?.ids.indexOf(id) ?? -1;
    if (!cur || at < 0) return;
    this.setList(cur.tab, listOf(select(this.listState(cur.tab), cur.ids, this.height, at)));
  }

  /** Selects row `index` (clamped) of the current tab. */
  selectIndex(index: number): void {
    const cur = this.current();
    if (!cur || cur.snap.status !== "ready") return;
    this.setList(cur.tab, listOf(select(this.listState(cur.tab), cur.ids, this.height, index)));
  }

  /** The row index of the first marked Item listed; -1 when none is. */
  firstMarkedIndex(): number {
    const { marks } = this.state;
    return marks.size ? this.currentIds().findIndex((id) => marks.has(id)) : -1;
  }

  /** Clears the marks, and V's range. */
  clearMarks(): void {
    this.ui.setState((s) => (s.marks.size || s.visual ? { ...s, marks: noMarks, visual: undefined } : s));
  }

  /**
   * After the rows shown changed under `/`: the selection stays on its Item
   * when it is still listed, else goes to the first row (Decision 9).
   */
  keepOrFirst(): void {
    const cur = this.current();
    if (!cur || cur.snap.status !== "ready") return;
    const list = this.listState(cur.tab);
    const kept = list.selectedId !== undefined && cur.ids.includes(list.selectedId);
    this.setList(cur.tab, listOf(kept ? reconcile(list, cur.ids, this.height) : select({ ...initialList }, cur.ids, this.height, 0)));
  }

  /** Puts back a tab's selection and scroll as they were (esc while typing `/`). */
  restoreList(tab: TabId, list: ListState): void {
    this.setList(tab, list);
    this.sync();
  }

  /** Re-settles the selection on the rows now shown. */
  resync(): void {
    this.sync();
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
   * After an action failed: the keys queued behind it that would act are
   * dropped, since they were meant for the state a success would have
   * left; the error toast says how many. Navigation keys queued first
   * (j, G, 2, …) still run: navigation keeps working through an outage
   * (Decision 6). In quick-add, text typed behind a failed capture is kept
   * as lines to capture next (typed text is never lost).
   */
  failed(text: string, opts: ToastOptions = {}): void {
    const queued = this.queue.take();
    let dropped = queued.length;
    const { mode } = this.state;
    if (mode.kind === "quickAdd" && queued.length) {
      const typed = typedLines(queued);
      dropped = typed.dropped;
      if (typed.lines.length) this.setMode({ ...mode, rest: [...(mode.rest ?? []), ...typed.lines] });
    } else if (mode.kind === "normal" && !mode.pending.length) {
      // They run now, before the toast, which the next key clears.
      const moves = this.leadingMoves(queued);
      for (const key of moves) void this.handle(key);
      dropped -= moves.length;
    }
    this.toast("error", dropped ? `${text} · ${dropped} key${dropped === 1 ? "" : "s"} dropped` : text, opts);
  }

  /** The keys at the start of `inputs` that only navigate, up to the first that would do anything else. */
  private leadingMoves(inputs: readonly Input[]): Input[] {
    let pending: KeyToken[] = [];
    let kept = 0;
    for (const [i, input] of inputs.entries()) {
      if (typeof input !== "string") break;
      const r = step(this.tries.normal, pending, input);
      if (r.kind === "pending") {
        pending = r.pending;
        continue;
      }
      if (r.kind !== "run" || !NAVIGATION.has(r.binding.action.type)) break;
      pending = [];
      kept = i + 1;
    }
    return inputs.slice(0, kept);
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
   * is not offered: it would undo that other op. `more` goes at the end.
   */
  toastOp(opId: string | undefined, fallback: string, more = ""): void {
    const top = this.store.undoState.getSnapshot().undo;
    this.toast("info", `${top && opId !== undefined && top.opId === opId ? `${singleLine(top.description)} · u undo` : fallback}${more}`);
  }

  /** A plain edit's toast: only when the Item left the current tab. */
  toastIfLeft(id: string, opId: string | undefined, fallback: string): void {
    if (!this.currentIds().includes(id)) this.toastOp(opId, fallback);
  }

  /** Shows a toast on the message line: info for 4 s, an error for 8 s or until the next key. */
  toast(level: "info" | "error", text: string, opts: ToastOptions = {}): void {
    const id = ++this.toastSeq;
    this.ui.setState((s) => ({ ...s, toast: { id, level, text, ...(opts.keepEnd && { keepEnd: true }) } }));
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

  /** A tab's selection and scroll. */
  listState(tab: TabId): ListState {
    return this.state.lists.get(tab) ?? initialList;
  }

  private setList(tab: TabId, list: ListState): void {
    this.ui.setState((s) => ({ ...s, lists: new Map(s.lists).set(tab, list) }));
  }

  private rows(tab: TabId, narrow?: string): { tab: TabId; snap: ViewSnapshot; ids: readonly string[] } {
    const snap = this.store.view(tabQuery(tab, narrow)).getSnapshot();
    let ids = this.idsCache.get(snap);
    if (!ids) this.idsCache.set(snap, (ids = snap.items.map((i) => i.id)));
    return { tab, snap, ids };
  }

  /** The current tab and the rows it lists: narrowed, while `/` narrows it. */
  private current(): { tab: TabId; snap: ViewSnapshot; ids: readonly string[] } | undefined {
    const { tab, narrow } = this.state;
    return tab === undefined ? undefined : this.rows(tab, narrow?.shown);
  }

  /** The current tab's own rows, never narrowed: the marks follow these. */
  private base(): { tab: TabId; snap: ViewSnapshot; ids: readonly string[] } | undefined {
    const { tab } = this.state;
    return tab === undefined ? undefined : this.rows(tab);
  }

  /**
   * After any store change: keeps the tab one that exists, observes its
   * view, drops the marks whose Item left it, and pins its selection, so
   * that a later change starts from where this one left it and background
   * changes never move the selection.
   */
  private sync(): void {
    const { views } = this.store.getViews();
    const { tab } = this.state;
    const first = views[0];
    if (first && (tab === undefined || (searchQuery(tab) === undefined && !views.some((v) => v.id === tab)))) {
      this.narrowing.release();
      this.ui.setState((s) => ({ ...s, ...leftTab(s), tab: first.id }));
    }
    const current = this.state.tab;
    if (this.watched?.tab !== current) {
      this.watched?.stop();
      // A Search tab's view goes as soon as the tab closes, as `/`'s do.
      const grace = current !== undefined && searchQuery(current) !== undefined ? { graceMs: 0 } : {};
      this.watched = current === undefined ? undefined : { tab: current, stop: this.store.view(tabQuery(current), grace).subscribe(() => {}) };
    }
    this.prune();
    const cur = this.current();
    // A view still loading (after a tab switch, say) has no rows yet; its selection waits for them.
    if (!cur || cur.snap.status !== "ready") return;
    const list = this.listState(cur.tab);
    const next = listOf(reconcile(list, cur.ids, this.height));
    if (!sameList(list, next)) this.setList(cur.tab, next);
  }

  /** Drops the marks whose Item the tab's own rows no longer hold (not those `/` only hides). */
  private prune(): void {
    const { marks, visual } = this.state;
    if (!marks.size && !visual) return;
    const base = this.base();
    if (!base || base.snap.status !== "ready") return;
    let present = this.idSets.get(base.snap);
    if (!present) this.idSets.set(base.snap, (present = new Set(base.ids)));
    const kept = pruneMarks(marks, present);
    const keptBase = visual && pruneMarks(visual.base, present);
    if (kept === marks && keptBase === visual?.base) return;
    this.ui.setState((s) => ({ ...s, marks: kept, ...(visual && keptBase && { visual: { ...visual, base: keptBase } }) }));
  }

  // --- keys ---

  private handle(input: Input): void | Promise<void> {
    if (this.state.quitting) return;
    if (this.state.toast?.level === "error") this.clearToast();
    const { mode } = this.state;
    if (typeof input !== "string") return this.onPaste(mode, input.paste);
    const alt = altKey(input);
    if (alt !== undefined) {
      // alt+X (Ink's meta): a word edit in a line being typed, never an esc
      // that would throw the line away (Alt+Left, Alt+Backspace); ESC then X
      // typed within Ink's 20 ms elsewhere, and for a character (hard case 3).
      const inLine = TEXT_MODES.has(mode.kind) ? altInLine(input) : "esc";
      if (inLine === "ignore") return;
      if (inLine === "esc") {
        this.queue.unshift([alt]);
        return this.handle("esc");
      }
    }
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
      case "search":
        return this.narrowing.key(mode, input);
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
      case "search":
        return this.narrowing.paste(mode, text);
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
      case "search":
        return this.openSearch(r.args);
    }
  }

  /**
   * :search q: the "Search: q" tab after the views (Decision 9), over
   * everything but deleted Items, most recently changed first. A new
   * search replaces it; leaving it closes it.
   */
  private openSearch(text: string): void {
    const query = matchText(text);
    if (query === undefined) return this.toast("info", "Search for what? :search <words>");
    if (Buffer.byteLength(query, "utf8") > 1024) return this.toast("error", "Search text is limited to 1024 bytes");
    const { tab, search } = this.state;
    const from = tab !== undefined && searchQuery(tab) !== undefined ? search?.from : tab;
    this.switchTab(searchTab(query), { query, ...(from !== undefined && { from }) });
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
        const tabs = this.tabs();
        const at = tabs.findIndex((t) => t === this.state.tab);
        return this.switchTab(tabs[(at + action.by + tabs.length) % tabs.length]);
      }
      case "focus_toggle":
        if (layout(this.state.size.columns, this.state.size.rows).wide) {
          return this.ui.setState((s) => ({ ...s, focus: s.focus === "list" ? "detail" : "list" }));
        }
        return this.state.overlay.kind === "detail" ? this.closeOverlay() : this.openDetail();
      case "open_detail":
        return this.openDetail();
      case "back":
        return this.back();
      case "help":
        return this.setMode({ kind: "help", scroll: 0 });
      case "help_close":
        return this.setMode(normalMode);
      case "command":
        return this.setMode({ kind: "command", line: emptyLine });
      case "quit":
        return this.quit();
      case "suspend":
        return this.suspendJob();
      case "narrow":
        return this.narrowing.open();
      case "mark":
        return this.mark();
      case "visual":
        return this.visual();
      case "edit_description":
        return this.editor.edit();
      case "quick_add":
        return action.child ? this.quickAdd.openChild() : this.quickAdd.open();
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
    const next = list(this.listState(cur.tab), cur.ids);
    this.setList(cur.tab, listOf(next));
    // V: the range follows the selection.
    const { visual } = this.state;
    if (visual) this.ui.setState((s) => ({ ...s, marks: rangeMarks(visual, cur.ids, next.index) }));
  }

  /** m: marks the selected Item, or unmarks it, and moves down (ending a V range, keeping its marks). */
  private mark(): void {
    const cur = this.current();
    const id = this.selectedId();
    if (!cur || id === undefined) return;
    this.ui.setState((s) => ({ ...s, visual: undefined, marks: toggleMark(s.marks, id) }));
    this.setList(cur.tab, listOf(move(this.listState(cur.tab), cur.ids, this.height, 1)));
  }

  /** V: starts marking the rows between here and the selection as it moves; V again ends it, keeping the marks. */
  private visual(): void {
    if (this.state.visual) return this.ui.setState((s) => ({ ...s, visual: undefined }));
    const cur = this.current();
    const id = this.selectedId();
    if (!cur || id === undefined) return;
    const visual = { anchorId: id, anchor: cur.ids.indexOf(id), base: this.state.marks };
    this.ui.setState((s) => ({ ...s, visual, marks: rangeMarks(visual, cur.ids, anchorIndex(visual, cur.ids)) }));
  }

  /**
   * esc in normal mode steps back one level per press (Decision 5): the
   * detail, then a V range (its marks kept), then the narrowing, then the
   * marks; on the Search tab with nothing else to undo, back to the tab it
   * was opened from.
   */
  private back(): void {
    const s = this.state;
    if (s.overlay.kind === "detail" || s.focus === "detail") return this.closeOverlay();
    if (s.visual) return this.ui.setState((st) => ({ ...st, visual: undefined }));
    if (s.narrow) return this.narrowing.clear();
    if (s.marks.size) return this.clearMarks();
    if (s.tab !== undefined && searchQuery(s.tab) !== undefined && s.search?.from !== undefined) this.switchTab(s.search.from);
  }

  /**
   * ctrl-z: stops the TUI as a shell job, the terminal handed back; fg
   * resumes it, redrawn, and the stream catches up. With no job control
   * nothing stops it, and it takes the terminal back at once.
   */
  private async suspendJob(): Promise<void> {
    const suspend = this.suspendTerminal;
    if (!suspend) return this.toast("info", "Cannot suspend here");
    let stopped = false;
    await suspend(async () => {
      stopped = await this.stopProcess();
    });
    if (!stopped) this.toast("info", "Cannot suspend: no job control here");
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

  /**
   * Goes to `tab`, with its own selection. The marks and the narrowing
   * belong to the tab left, and go; so does the Search tab, when left.
   */
  private switchTab(tab: TabId | undefined, search?: UiState["search"]): void {
    if (tab === undefined || (tab === this.state.tab && !search)) return;
    this.narrowing.release();
    this.ui.setState((s) => {
      const left = leftTab(s);
      // A new search starts from its first row.
      const lists = search && left.lists.has(tab) ? new Map([...left.lists].filter(([t]) => t !== tab)) : left.lists;
      return { ...s, ...left, lists, tab, focus: "list", overlay: { kind: "none" }, search };
    });
    this.sync();
  }

  /** The tabs, in order: the views, then the Search tab while open. */
  private tabs(): TabId[] {
    const { search } = this.state;
    return [...this.store.getViews().views.map((v) => v.id), ...(search ? [searchTab(search.query)] : [])];
  }

  private clearToast(): void {
    if (this.toastTimer) clearTimeout(this.toastTimer);
    this.toastTimer = undefined;
    this.ui.setState((s) => ({ ...s, toast: undefined }));
  }
}

/** What leaving a tab clears: its marks, its V range and its narrowing; a Search tab's selection too, as the tab closes. */
function leftTab(s: UiState): Pick<UiState, "lists" | "marks" | "visual" | "narrow"> {
  const lists = s.tab !== undefined && searchQuery(s.tab) !== undefined ? new Map([...s.lists].filter(([t]) => t !== s.tab)) : s.lists;
  return { lists, marks: noMarks, visual: undefined, narrow: undefined };
}
