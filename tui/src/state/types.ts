// The TUI's own state (Decision 2): what is selected, which mode keys go
// to, what the message line says. Server data stays in LiveStore.

import type { CaptureDraft } from "@todo-tui/client";
import type { PromptKind } from "../keys/keymap.js";
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
      /** `w`: the people already waited on, for tab. */
      readonly people?: readonly string[];
    }
  | { readonly kind: "picker"; readonly picker: "status"; readonly target: Target; readonly index: number }
  /** `dd`: y confirms, any other key cancels and is swallowed. */
  | { readonly kind: "confirm"; readonly text: string; readonly action: "delete"; readonly target: Target }
  | { readonly kind: "command"; readonly line: LineState }
  /** `/`: the text narrowing the tab, live as it is typed. */
  | { readonly kind: "search"; readonly line: LineState }
  /** `pending`: a prefix of one of help's own bindings (gg), when one is typed. */
  | { readonly kind: "help"; readonly scroll: number; readonly pending?: readonly KeyToken[] };

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
  readonly quitting: boolean;
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
});
