// The TUI's own state (Decision 2): what is selected, which mode keys go
// to, what the message line says. Server data stays in LiveStore.

import type { CaptureDraft } from "@todo-tui/client";
import type { Action, PromptKind } from "../keys/keymap.js";
import type { LineState } from "../keys/line-editor.js";
import type { KeyToken } from "../keys/tokenize.js";
import type { ListState } from "./selection.js";

/** A saved view's id; `search:<q>` for a :search tab (phase 3). */
export type TabId = string;

/** What an action acts on, frozen when it starts: the marked ids in list order, else the selected one. */
export interface Target {
  readonly ids: readonly string[];
  readonly bulk: boolean;
  /** Revisions as rendered; sent only for edits made from a rendered value (Decision 6). */
  readonly revisions: ReadonlyMap<string, bigint>;
}

/** One input mode at a time owns the keys (m4_arch §1.1). */
export type Mode =
  /** `pending`: a prefix typed so far, such as ["g"] or ["d"]. */
  | { readonly kind: "normal"; readonly pending: readonly KeyToken[] }
  | {
      readonly kind: "quickAdd";
      readonly parentId?: string;
      readonly line: LineState;
      readonly preview?: { readonly text: string; readonly draft: CaptureDraft };
      readonly error?: string;
    }
  | {
      readonly kind: "prompt";
      readonly prompt: PromptKind;
      readonly target: Target;
      readonly line: LineState;
      /** The field value the edit started from, for the rebase on a conflict. */
      readonly base?: unknown;
      readonly preview?: string;
      readonly error?: string;
    }
  | { readonly kind: "picker"; readonly picker: "status"; readonly target: Target; readonly index: number }
  /** `dd`: y confirms, any other key cancels. */
  | { readonly kind: "confirm"; readonly text: string; readonly onYes: Action }
  | { readonly kind: "command"; readonly line: LineState }
  | { readonly kind: "search"; readonly line: LineState }
  /** `pending`: a prefix of one of help's own bindings (gg), when one is typed. */
  | { readonly kind: "help"; readonly scroll: number; readonly pending?: readonly KeyToken[] };

export type ModeKind = Mode["kind"];

/** The full-screen detail of narrow layouts, orthogonal to the mode. */
export type Overlay = { readonly kind: "none" } | { readonly kind: "detail" };

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
  readonly mode: Mode;
  readonly overlay: Overlay;
  /** Which pane j/k act on. */
  readonly focus: "list" | "detail";
  /** The detail pane's scroll, for the Item it shows; its content height, as last laid out. */
  readonly detail: { readonly id?: string; readonly scroll: number; readonly lines: number };
  /** The message line: one toast, newest wins. */
  readonly toast?: Toast;
  readonly size: { readonly columns: number; readonly rows: number };
  readonly quitting: boolean;
}

export const normalMode: Mode = { kind: "normal", pending: [] };

export const initialUiState = (size = { columns: 80, rows: 24 }): UiState => ({
  lists: new Map(),
  mode: normalMode,
  overlay: { kind: "none" },
  focus: "list",
  detail: { scroll: 0, lines: 0 },
  size,
  quitting: false,
});
