// The keymap (design 2.5, plan Decision 5): one declarative table with
// stable snake_case action ids, which M6's overrides will name. The
// dispatcher compiles it to a trie per mode and the help overlay is
// generated from it, so the two cannot disagree.

import { Priority, Status } from "@todo-tui/client";
import type { KeyToken } from "./tokenize.js";

export type PromptKind = "title" | "labels" | "due" | "defer" | "waiting";

export type Action =
  | { type: "move"; by: 1 | -1 }
  | { type: "top" }
  | { type: "bottom" }
  | { type: "half_page"; dir: 1 | -1 }
  | { type: "view"; position: number }
  | { type: "view_step"; by: 1 | -1 }
  | { type: "focus_toggle" }
  | { type: "open_detail" }
  | { type: "back" }
  | { type: "help" }
  | { type: "command" }
  | { type: "quit" }
  // Phase 2 and 3 of M4.
  | { type: "narrow" }
  | { type: "quick_add"; child: boolean }
  | { type: "prompt"; prompt: PromptKind }
  | { type: "edit_description" }
  | { type: "set_priority"; priority: Priority }
  | { type: "status_picker" }
  | { type: "set_status"; status: Status }
  | { type: "clear_waiting" }
  | { type: "archive_toggle" }
  | { type: "delete" }
  | { type: "mark" }
  | { type: "visual" }
  | { type: "undo" }
  | { type: "redo" }
  | { type: "help_close" };

/** The modes with a keymap; the text modes (command, quick-add, prompts, search) take their keys from the line editor. */
export type KeymapMode = "normal" | "help";

export type Section = "Navigation" | "Views" | "Items" | "Marks" | "Undo" | "App" | "Help";

export interface Binding {
  /** Stable snake_case id, e.g. "set_priority_1". */
  readonly id: string;
  /** Alternative key sequences, the tokens of each separated by spaces: "j", "g g", "ctrl+d". */
  readonly keys: readonly string[];
  readonly action: Action;
  /** The modes it is bound in (default normal). */
  readonly modes?: readonly KeymapMode[];
  /** With marks in the tab, it acts on every marked Item (Decision 8). */
  readonly markAware?: boolean;
  readonly section: Section;
  readonly help: string;
  /** Help lists bindings that share this key label once, under it ("1–9"). */
  readonly helpKeys?: string;
  /** Whether it can run now: help leaves it out otherwise, and pressing it says so. */
  readonly available?: () => boolean;
}

/** Bindings whose actions arrive in a later phase of M4. */
const later = () => false;

const priorities: readonly [string, Priority][] = [
  ["1", Priority.HIGHEST],
  ["2", Priority.HIGH],
  ["3", Priority.MEDIUM],
  ["4", Priority.LOW],
  ["5", Priority.LOWEST],
];

export const BINDINGS: readonly Binding[] = [
  { id: "move_down", keys: ["j", "down"], action: { type: "move", by: 1 }, section: "Navigation", help: "Next row (detail focused: scroll)" },
  { id: "move_up", keys: ["k", "up"], action: { type: "move", by: -1 }, section: "Navigation", help: "Previous row (detail focused: scroll)" },
  { id: "top", keys: ["g g", "home"], action: { type: "top" }, section: "Navigation", help: "First row" },
  { id: "bottom", keys: ["G", "end"], action: { type: "bottom" }, section: "Navigation", help: "Last row" },
  { id: "half_page_down", keys: ["ctrl+d", "pagedown"], action: { type: "half_page", dir: 1 }, section: "Navigation", help: "Half a page down" },
  { id: "half_page_up", keys: ["ctrl+u", "pageup"], action: { type: "half_page", dir: -1 }, section: "Navigation", help: "Half a page up" },
  { id: "focus_toggle", keys: ["tab"], action: { type: "focus_toggle" }, section: "Navigation", help: "Focus list ↔ detail" },
  { id: "open_detail", keys: ["enter"], action: { type: "open_detail" }, section: "Navigation", help: "Open the detail" },
  { id: "back", keys: ["esc"], action: { type: "back" }, section: "Navigation", help: "Back: close the detail, clear" },
  ...Array.from({ length: 9 }, (_, i): Binding => ({
    id: `view_${i + 1}`,
    keys: [String(i + 1)],
    action: { type: "view", position: i + 1 },
    section: "Views",
    help: "Go to view N",
    helpKeys: "1–9",
  })),
  { id: "view_prev", keys: ["["], action: { type: "view_step", by: -1 }, section: "Views", help: "Previous view" },
  { id: "view_next", keys: ["]"], action: { type: "view_step", by: 1 }, section: "Views", help: "Next view" },
  { id: "narrow", keys: ["/"], action: { type: "narrow" }, section: "Views", help: "Narrow this view", available: later },

  { id: "quick_add", keys: ["a"], action: { type: "quick_add", child: false }, section: "Items", help: "Quick-add", available: later },
  { id: "quick_add_child", keys: ["A"], action: { type: "quick_add", child: true }, section: "Items", help: "Quick-add a subtask", available: later },
  { id: "edit_title", keys: ["i"], action: { type: "prompt", prompt: "title" }, section: "Items", help: "Edit the title", available: later },
  { id: "edit_description", keys: ["E"], action: { type: "edit_description" }, section: "Items", help: "Edit the description in $EDITOR", available: later },
  ...priorities.map(
    ([key, priority]): Binding => ({
      id: `set_priority_${key}`,
      keys: [`p ${key}`],
      action: { type: "set_priority", priority },
      markAware: true,
      section: "Items",
      help: "Set the priority",
      helpKeys: "p1–p5",
      available: later,
    }),
  ),
  { id: "clear_priority", keys: ["p 0"], action: { type: "set_priority", priority: Priority.NONE }, markAware: true, section: "Items", help: "Clear the priority", available: later },
  { id: "set_due", keys: ["^"], action: { type: "prompt", prompt: "due" }, markAware: true, section: "Items", help: "Set the due date", available: later },
  { id: "defer", keys: ["D"], action: { type: "prompt", prompt: "defer" }, markAware: true, section: "Items", help: "Defer until…", available: later },
  { id: "set_waiting", keys: ["w"], action: { type: "prompt", prompt: "waiting" }, markAware: true, section: "Items", help: "Waiting on…", available: later },
  { id: "clear_waiting", keys: ["W"], action: { type: "clear_waiting" }, markAware: true, section: "Items", help: "Clear waiting", available: later },
  { id: "set_status", keys: ["s"], action: { type: "status_picker" }, markAware: true, section: "Items", help: "Set the status", available: later },
  { id: "mark_done", keys: ["x"], action: { type: "set_status", status: Status.DONE }, markAware: true, section: "Items", help: "Mark done", available: later },
  { id: "mark_cancelled", keys: ["X"], action: { type: "set_status", status: Status.CANCELLED }, markAware: true, section: "Items", help: "Mark cancelled", available: later },
  { id: "edit_labels", keys: ["t"], action: { type: "prompt", prompt: "labels" }, markAware: true, section: "Items", help: "Edit the labels", available: later },
  { id: "toggle_archive", keys: ["z"], action: { type: "archive_toggle" }, markAware: true, section: "Items", help: "Archive / unarchive", available: later },
  { id: "delete", keys: ["d d"], action: { type: "delete" }, markAware: true, section: "Items", help: "Delete (confirm)", available: later },

  { id: "mark", keys: ["m"], action: { type: "mark" }, section: "Marks", help: "Mark and move down", available: later },
  { id: "visual_mark", keys: ["V"], action: { type: "visual" }, section: "Marks", help: "Mark a range", available: later },

  { id: "undo", keys: ["u"], action: { type: "undo" }, section: "Undo", help: "Undo", available: later },
  { id: "redo", keys: ["ctrl+r"], action: { type: "redo" }, section: "Undo", help: "Redo", available: later },

  { id: "command", keys: [":"], action: { type: "command" }, section: "App", help: "Command line" },
  { id: "help", keys: ["?", "fkey"], action: { type: "help" }, section: "App", help: "This help" },
  { id: "quit", keys: ["q", "ctrl+c"], action: { type: "quit" }, section: "App", help: "Quit" },

  // In help, the navigation actions scroll it.
  { id: "help_down", keys: ["j", "down"], action: { type: "move", by: 1 }, modes: ["help"], section: "Help", help: "Scroll down" },
  { id: "help_up", keys: ["k", "up"], action: { type: "move", by: -1 }, modes: ["help"], section: "Help", help: "Scroll up" },
  { id: "help_half_page_down", keys: ["ctrl+d", "pagedown", "space"], action: { type: "half_page", dir: 1 }, modes: ["help"], section: "Help", help: "Half a page down" },
  { id: "help_half_page_up", keys: ["ctrl+u", "pageup"], action: { type: "half_page", dir: -1 }, modes: ["help"], section: "Help", help: "Half a page up" },
  { id: "help_top", keys: ["g g", "home"], action: { type: "top" }, modes: ["help"], section: "Help", help: "Top" },
  { id: "help_bottom", keys: ["G", "end"], action: { type: "bottom" }, modes: ["help"], section: "Help", help: "Bottom" },
  { id: "help_close", keys: ["esc", "q", "?", "fkey", "ctrl+c"], action: { type: "help_close" }, modes: ["help"], section: "Help", help: "Close" },
];

export const isAvailable = (b: Binding): boolean => b.available?.() ?? true;

export const bindingModes = (b: Binding): readonly KeymapMode[] => b.modes ?? ["normal"];

/** A binding's key sequences as tokens. */
export const sequences = (b: Binding): KeyToken[][] => b.keys.map((k) => k.split(" "));
