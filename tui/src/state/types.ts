// The TUI's own state (Decision 2): what is selected, which mode keys go
// to, what the message line says. Server data stays in LiveStore.

import type { CaptureDraft } from "@todo-tui/client";
import { BINDINGS, type Binding, type PromptKind } from "../keys/keymap.js";
import type { KeymapWarning } from "../keys/overrides.js";
import type { LineState } from "../keys/line-editor.js";
import type { KeyToken } from "../keys/tokenize.js";
import type { ListState } from "./selection.js";

/** A saved view's id; `search:<q>` for the :search tab (state/tabs.ts). */
export type TabId = string;

/** What an action acts on, frozen when it starts: the marked ids in list order, else the selected one. */
export interface Target {
  readonly ids: readonly string[];
  readonly bulk: boolean;
  /** Revisions as rendered; sent only for edits made from a rendered value (Decision 6). */
  readonly revisions: ReadonlyMap<string, bigint>;
}

/** What a prompt's text resolves to, shown under it: "→ Fri Oct 30", or a warning. */
export interface PromptPreview {
  /** The text it previews: a response for other text is stale. */
  readonly for: string;
  readonly text: string;
  /** A warning (red): enter keeps the prompt open. */
  readonly warn: boolean;
}

/** One name `w` can complete to, and what enter saves for it. */
export interface WaitingChoice {
  readonly name: string;
  /** A known Person's id. */
  readonly personId?: string;
  /** A directory user's, not yet a Person: enter adds them first. */
  readonly directory?: { readonly instanceId: string; readonly accountId: string; readonly email: string };
  /** The address shown in the hint. */
  readonly email?: string;
}

/** What a :view prompt's text parses to, shown under it. */
export interface QueryPreview {
  readonly for: string;
  /** "→ 12 items", or the first error. */
  readonly text: string;
  readonly warn: boolean;
  /** The first error's span, in code points, coloured in the line. */
  readonly span?: { readonly start: number; readonly end: number };
}

/** One input mode at a time owns the keys (m4_arch §1.1). */
export type Mode =
  /** `pending`: a prefix typed so far, such as ["g"] or ["d"]. */
  | { readonly kind: "normal"; readonly pending: readonly KeyToken[] }
  | {
      readonly kind: "quickAdd";
      /** A: captures as a child of this Item, named as `parentTitle` above the line. */
      readonly parentId?: string;
      readonly parentTitle?: string;
      readonly line: LineState;
      /** ParseCapture's draft for `text`; shown only while `text` is what is typed. */
      readonly preview?: { readonly text: string; readonly draft: CaptureDraft };
      /** Lines still to capture, after one failed: each comes into the line once the one before is saved. */
      readonly rest?: readonly string[];
    }
  | {
      readonly kind: "prompt";
      readonly prompt: PromptKind;
      /** Frozen when the prompt opens (hard case 46). */
      readonly target: Target;
      readonly line: LineState;
      /**
       * For the edits made from a rendered value (i, t): the field as the
       * edit started from it and the revision sent as expected_revision;
       * both move to the server's after a conflict (Decision 6).
       */
      readonly base?: { readonly value: string; readonly revision: bigint };
      readonly preview?: PromptPreview;
      /** Why enter did not save, shown instead of the preview until the next edit. */
      readonly error?: string;
      /** The field's value on the server after a conflict, which the next enter overwrites. */
      readonly conflict?: string;
      /**
       * `w`: who tab completes from: the People the daemon knows, then the
       * free text already waited on, then (after a tab found no one here)
       * Jira's directory (design 3.5).
       */
      readonly people?: readonly WaitingChoice[];
    }
  | { readonly kind: "picker"; readonly picker: "status"; readonly target: Target; readonly index: number }
  /** `dd`: y confirms, any other key cancels and is swallowed. */
  | { readonly kind: "confirm"; readonly text: string; readonly action: "delete"; readonly target: Target }
  /** `:view delete`: as dd's, for a view. */
  | { readonly kind: "confirm"; readonly text: string; readonly action: "delete_view"; readonly viewId: string; readonly name: string }
  /**
   * `:view new` / `:view edit`: query text (design 4.8) for a view, its
   * ParseQuery preview below it.
   */
  | {
      readonly kind: "query";
      /** The view edited; unset for a new one. */
      readonly viewId?: string;
      readonly name: string;
      /** The view's revision as the edit started from it, sent as expected_revision. */
      readonly revision?: bigint;
      /** The text the edit started from: what the view held. */
      readonly base?: string;
      readonly line: LineState;
      /** ParseQuery's answer for `for`: the match count, or the first error and its span. */
      readonly preview?: QueryPreview;
      /** Why enter did not save. */
      readonly error?: string;
      /** The view's query on the server after a conflict, which the next enter overwrites. */
      readonly conflict?: string;
    }
  /** `back`: the pane it was opened from, which :w applies and esc returns to. */
  | { readonly kind: "command"; readonly line: LineState; readonly back?: Extract<Mode, { kind: "suggestions" }> }
  /** `/`: the text narrowing the tab, live as it is typed. */
  | { readonly kind: "search"; readonly line: LineState }
  /**
   * `pending`: a prefix of one of help's own bindings (gg), when one is
   * typed. `/` narrows the help to `query`'s rows: `line` while it is
   * typed, `saved` the scroll from before, which esc goes back to.
   */
  | {
      readonly kind: "help";
      readonly scroll: number;
      readonly pending?: readonly KeyToken[];
      readonly query?: string;
      readonly line?: LineState;
      readonly saved?: number;
    }
  /** :l48: the report's text, scrolled; `y` copies it. */
  | { readonly kind: "report"; readonly scroll: number; readonly text: string; readonly pending?: readonly KeyToken[] }
  /**
   * S: the Suggestions pane (design 2.6). `cursor` is a row of paneRows;
   * `ticked` the patch ids enter ticked, applied by ctrl-s.
   */
  | {
      readonly kind: "suggestions";
      readonly cursor: number;
      /** The row the cursor is on (rowKey): it follows that row when others come or go. */
      readonly at?: string;
      readonly ticked: ReadonlySet<string>;
      readonly pending?: readonly KeyToken[];
    }
  /**
   * An edit of someone else's issue (design 5.9): y sends it again with
   * confirm_not_mine; any other key drops it. `back` is the mode to return
   * to (the pane), else normal.
   */
  | { readonly kind: "confirm"; readonly text: string; readonly action: "not_mine"; readonly back?: Mode };

export type ModeKind = Mode["kind"];

/** The full-screen detail of narrow layouts, orthogonal to the mode. */
export type Overlay = { readonly kind: "none" } | { readonly kind: "detail" };

/**
 * `/` narrowing the current tab (Decision 9): the list shows the tab's
 * rows that match `shown`, once ListItems has answered for it.
 */
export interface Narrow {
  /** The text asked for last (trimmed); undefined for blank or token-less text, which narrows nothing. */
  readonly want?: string;
  /** The text whose rows the list shows; undefined while it shows the tab's own rows. */
  readonly shown?: string;
  /** The tab's selection and scroll before narrowing: esc while typing goes back to it. */
  readonly saved: ListState;
}

/** V: marking the rows between the one it started on and the selection (Decision 8). */
export interface Visual {
  readonly anchorId: string;
  /** The anchor's index, for when its Item leaves the list. */
  readonly anchor: number;
  /** The marks from before V, which the range adds to. */
  readonly base: ReadonlySet<string>;
}

export interface Toast {
  readonly id: number;
  readonly level: "info" | "error";
  readonly text: string;
  /** Cut in the middle, not at the end, when it does not fit: its end (a file's name) is what it is for. */
  readonly keepEnd?: boolean;
}

export interface UiState {
  /** The current tab, once the views are known. */
  readonly tab?: TabId;
  /** Selection and scroll per tab. */
  readonly lists: ReadonlyMap<TabId, ListState>;
  /** The current tab's marked Items (Decision 8); cleared on a tab switch. */
  readonly marks: ReadonlySet<string>;
  readonly visual?: Visual;
  /** The current tab's `/` narrowing; cleared on a tab switch. */
  readonly narrow?: Narrow;
  /** The :search tab, after the views, while open: its query, and the tab esc goes back to. */
  readonly search?: { readonly query: string; readonly from?: TabId };
  readonly mode: Mode;
  readonly overlay: Overlay;
  /** Which pane j/k act on. */
  readonly focus: "list" | "detail";
  /** The detail pane's scroll, for the Item it shows; its content height, as last laid out. */
  readonly detail: { readonly id?: string; readonly scroll: number; readonly lines: number };
  /** The message line: one toast, newest wins. */
  readonly toast?: Toast;
  readonly size: { readonly columns: number; readonly rows: number };
  /** An action that has run for a while: "saving…" in the status bar. */
  readonly busy?: string;
  /** How many refines Claude is working on (space r): "✦ refining…". */
  readonly refining?: number;
  readonly quitting: boolean;
  /** The keymap in effect: the defaults with config's [keymap] overrides, and what was wrong with them. */
  readonly keymap: { readonly bindings: readonly Binding[]; readonly warnings: readonly KeymapWarning[] };
}

export const normalMode: Mode = { kind: "normal", pending: [] };

export const noMarks: ReadonlySet<string> = new Set();

export const initialUiState = (size = { columns: 80, rows: 24 }): UiState => ({
  lists: new Map(),
  marks: noMarks,
  mode: normalMode,
  overlay: { kind: "none" },
  focus: "list",
  detail: { scroll: 0, lines: 0 },
  size,
  quitting: false,
  keymap: { bindings: BINDINGS, warnings: [] },
});
