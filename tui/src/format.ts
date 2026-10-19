import { Priority, Status, type Item } from "@todo-tui/client";

const glyphs: Partial<Record<Status, string>> = {
  [Status.INBOX]: "○",
  [Status.NEXT]: "○",
  [Status.SOMEDAY]: "○",
  [Status.IN_PROGRESS]: "◐",
  [Status.DONE]: "●",
  [Status.CANCELLED]: "⊘",
};

const priorities: Partial<Record<Priority, string>> = {
  [Priority.HIGHEST]: "+1",
  [Priority.HIGH]: "+2",
  [Priority.MEDIUM]: "+3",
  [Priority.LOW]: "+4",
  [Priority.LOWEST]: "+5",
};

export const statusGlyph = (status: Status): string => glyphs[status] ?? "?";

export function priorityLabel(item: Item): string {
  const label = priorities[item.effectivePriority] ?? "";
  return item.localPriorityOverride === undefined || !label ? label : `${label}*`;
}

/** Collapses whitespace so a title always renders on one line. */
export const singleLine = (text: string): string => text.replace(/\s+/g, " ").trim();
