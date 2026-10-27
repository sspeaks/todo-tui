// `/` (Decision 9): narrows the current tab on the server as the text is
// typed, ListItems{view_id, filter any_of[title, labels, remote_link.key
// MATCHES text]}, with one request in flight and the latest text winning.
// The tab badge keeps the tab's own count. enter keeps the narrowing, esc
// while typing goes back to the tab's rows and selection, and a tab switch
// clears it. Blank or token-less text narrows nothing; the text is capped
// at MATCHES's 1024 bytes.

import { type LiveStore, type LiveView, MAX_MATCHES_BYTES } from "@todo-tui/client";
import { singleLine } from "../format/text.js";
import { editLine, insert, type LineState, lineOf } from "../keys/line-editor.js";
import { isChar, type KeyToken } from "../keys/tokenize.js";
import { matchText, tabQuery } from "../state/tabs.js";
import { type Mode, normalMode, type TabId } from "../state/types.js";
import type { Controller } from "./controller.js";
import { Latest } from "./latest.js";

type Search = Extract<Mode, { kind: "search" }>;

/** Inserts `text` at the cursor, as much of it as keeps the line within MATCHES's limit. */
export function insertCapped(line: LineState, text: string): LineState {
  let room = Math.max(0, MAX_MATCHES_BYTES - Buffer.byteLength(line.text, "utf8"));
  let kept = "";
  for (const cp of text) {
    const n = Buffer.byteLength(cp, "utf8");
    if (n > room) break;
    room -= n;
    kept += cp;
  }
  return insert(line, kept);
}

/** Resolves once `view` has answered its first ListItems, or after `ms`. */
function firstLoad(store: LiveStore, view: LiveView, ms: number): Promise<void> {
  return new Promise((resolve) => {
    let unsubscribe = () => {};
    const timer = setTimeout(done, ms);
    function done() {
      clearTimeout(timer);
      unsubscribe();
      resolve();
    }
    const check = () => {
      if (view.getSnapshot().status !== "loading") done();
    };
    unsubscribe = store.subscribe(check);
    check();
  });
}

export class Narrowing {
  private readonly latest: Latest<{ tab: TabId; text: string }, () => void>;
  /** The narrowed view the list shows, observed so that it stays live; dropped at once when let go. */
  private held: { tab: TabId; text: string; stop: () => void } | undefined;

  constructor(private readonly c: Controller) {
    this.latest = new Latest(
      async ({ tab, text }) => {
        const view = c.store.view(tabQuery(tab, text), { graceMs: 0 });
        const stop = view.subscribe(() => {});
        await firstLoad(c.store, view, c.timing.rpcTimeoutMs);
        return stop;
      },
      ({ tab, text }, stop) => {
        // An answer for text since changed (or another tab) is never shown.
        if (c.state.tab !== tab || c.state.narrow?.want !== text) return stop();
        this.show(tab, text, stop);
      },
    );
  }

  /** `/`: the search line, holding the narrowing's text when one is kept; the list comes to the front, as it is what narrows. */
  open(): void {
    const { tab, narrow } = this.c.state;
    if (tab === undefined) return;
    const saved = narrow?.saved ?? this.c.listState(tab);
    this.c.ui.setState((s) => ({
      ...s,
      visual: undefined,
      narrow: { ...narrow, saved },
      mode: { kind: "search", line: lineOf(narrow?.shown ?? "") },
      overlay: { kind: "none" },
      focus: "list",
    }));
  }

  key(mode: Search, key: KeyToken): void | Promise<void> {
    switch (key) {
      case "esc":
      case "ctrl+c":
        return this.cancel();
      case "enter":
        return this.commit();
      case "tab":
        return;
      case "backspace":
        // As in vim, backspace on an empty search leaves it.
        if (mode.line.text === "") return this.cancel();
    }
    const line = isChar(key) ? insertCapped(mode.line, this.c.charRun(key)) : editLine(mode.line, key);
    if (line) this.edit(line);
  }

  paste(mode: Search, text: string): void {
    this.edit(insertCapped(mode.line, singleLine(text)));
  }

  /** Resolves once the list shows the latest text's rows (tests). */
  idle(): Promise<void> {
    return this.latest.idle();
  }

  /** Lets go of the narrowed view (a tab switch, or narrowing cleared). */
  release(): void {
    this.held?.stop();
    this.held = undefined;
  }

  /** Clears the narrowing, the selection staying on its Item (esc in normal mode). */
  clear(): void {
    this.release();
    this.c.ui.setState((s) => (s.narrow ? { ...s, narrow: undefined } : s));
    this.c.resync();
  }

  private edit(line: LineState): void {
    this.c.setMode({ kind: "search", line });
    const want = matchText(line.text);
    const { tab, narrow } = this.c.state;
    if (tab === undefined || !narrow) return;
    if (want === undefined) {
      // Nothing to match on: the tab's own rows, at once.
      this.release();
      this.c.ui.setState((s) => ({ ...s, narrow: { saved: narrow.saved } }));
      return this.c.keepOrFirst();
    }
    this.c.ui.setState((s) => ({ ...s, narrow: { ...narrow, want } }));
    if (want !== narrow.shown) this.latest.request({ tab, text: want });
  }

  private show(tab: TabId, text: string, stop: () => void): void {
    this.held?.stop();
    this.held = { tab, text, stop };
    this.c.ui.setState((s) => (s.narrow ? { ...s, narrow: { ...s.narrow, shown: text } } : s));
    this.c.keepOrFirst();
  }

  /** esc while typing: the tab's own rows again, with the selection from before `/`. */
  private cancel(): void {
    const { tab, narrow } = this.c.state;
    this.release();
    this.c.ui.setState((s) => ({ ...s, narrow: undefined, mode: normalMode }));
    if (tab !== undefined && narrow) this.c.restoreList(tab, narrow.saved);
  }

  /**
   * enter: keeps the narrowing, once the list shows the text's rows, so
   * that a key typed after enter acts on them; text that narrows nothing
   * clears it.
   */
  private async commit(): Promise<void> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    await Promise.race([this.latest.idle(), new Promise<void>((resolve) => (timer = setTimeout(resolve, this.c.timing.rpcTimeoutMs)))]);
    clearTimeout(timer);
    this.c.setMode(normalMode);
    if (this.c.state.narrow?.want === undefined) this.clear();
  }
}
