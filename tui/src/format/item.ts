// How an Item reads in a list row and in the detail pane (design 2.2).

import { CreatedVia, type Item, type PersonRef, Priority, Status } from "@todo-tui/client";
import { dayOf, shortDay } from "./dates.js";
import { displayWidth, fit, singleLine, truncate } from "./text.js";

const glyphs: Partial<Record<Status, string>> = {
  [Status.INBOX]: "○",
  [Status.NEXT]: "○",
  [Status.SOMEDAY]: "○",
  [Status.IN_PROGRESS]: "◐",
  [Status.DONE]: "●",
  [Status.CANCELLED]: "⊘",
};

const statusNames: Partial<Record<Status, string>> = {
  [Status.INBOX]: "Inbox",
  [Status.NEXT]: "Next",
  [Status.SOMEDAY]: "Someday",
  [Status.IN_PROGRESS]: "In progress",
  [Status.DONE]: "Done",
  [Status.CANCELLED]: "Cancelled",
};

const priorities: Partial<Record<Priority, string>> = {
  [Priority.HIGHEST]: "+1",
  [Priority.HIGH]: "+2",
  [Priority.MEDIUM]: "+3",
  [Priority.LOW]: "+4",
  [Priority.LOWEST]: "+5",
};

const priorityNames: Partial<Record<Priority, string>> = {
  [Priority.HIGHEST]: "highest",
  [Priority.HIGH]: "high",
  [Priority.MEDIUM]: "medium",
  [Priority.LOW]: "low",
  [Priority.LOWEST]: "lowest",
};

const createdVia: Partial<Record<CreatedVia, string>> = {
  [CreatedVia.TUI]: "tui",
  [CreatedVia.CLI]: "cli",
  [CreatedVia.MCP]: "mcp",
  [CreatedVia.REMOTE_IMPORT]: "import",
  [CreatedVia.RESTORE]: "restore",
  [CreatedVia.SPLIT]: "split",
  [CreatedVia.RECURRENCE]: "recurrence",
};

export const statusGlyph = (status: Status): string => glyphs[status] ?? "?";

export const statusName = (status: Status): string => statusNames[status] ?? "?";

/** "tui", "cli", …; "" when unknown. */
export const createdViaName = (via: CreatedVia): string => createdVia[via] ?? "";

/** The effective priority, "+1" to "+5", with "*" when the local override set it; "" for none. */
export function priorityLabel(item: Item): string {
  const label = priorities[item.effectivePriority] ?? "";
  return item.localPriorityOverride === undefined || !label ? label : `${label}*`;
}

/** "+3 medium", or "none". */
export const priorityName = (p: Priority): string => (priorities[p] ? `${priorities[p]} ${priorityNames[p]}` : "none");

/** A person as text: the free text, or the Person's id until People exist (P2). */
export function personName(p: PersonRef | undefined): string {
  if (p?.ref.case === "freeText") return singleLine(p.ref.value);
  if (p?.ref.case === "personId") return singleLine(p.ref.value);
  return "";
}

/** The short id the CLI prints: the last 8 characters of the ULID. */
export const shortId = (id: string): string => id.slice(-8);

/** The cells of a list row after the title; empty when unset. */
export interface RowCells {
  /** The remote key (P2). */
  key: string;
  /** "^Fri"; red when overdue. */
  due: string;
  overdue: boolean;
  /** "⧗dana 3d"; highlighted once the follow-up is due. */
  waiting: string;
  /** The age at the end of `waiting` (" 3d"), kept when the name is cut. */
  waitingAge: string;
  followUpDue: boolean;
  /** ⏸date when deferred, ↑ pending push, ! conflict or push failure, 🔁 recurring. */
  markers: string;
}

export function rowCells(item: Item, today: number): RowCells {
  const waiting = item.waiting;
  let waitingText = "";
  let age = "";
  if (waiting) {
    age = waiting.since ? ` ${Math.max(0, today - dayOf(waiting.since))}d` : "";
    waitingText = `⧗${personName(waiting.on)}${age}`;
  }
  const markers: string[] = [];
  if (item.deferUntil && dayOf(item.deferUntil) > today) markers.push(`⏸${shortDay(dayOf(item.deferUntil), today)}`);
  if (item.pendingFields.length) markers.push("↑");
  if (item.hasConflict || item.hasPushFailure) markers.push("!");
  if (item.recurrence) markers.push("🔁");
  return {
    key: singleLine(item.remoteLink?.key ?? ""),
    due: item.dueDate ? `^${shortDay(dayOf(item.dueDate), today)}` : "",
    overdue: item.isOverdue,
    waiting: waitingText,
    waitingAge: age,
    followUpDue: waiting?.followUpOn !== undefined && dayOf(waiting.followUpOn) <= today,
    markers: markers.join(" "),
  };
}

/** The waiting cell in exactly `width` cells: a long name cut, the age kept ("⧗the acco… 3d"). */
export function fitWaiting(cells: Pick<RowCells, "waiting" | "waitingAge">, width: number): string {
  const { waiting, waitingAge: age } = cells;
  if (!age || displayWidth(waiting) <= width) return fit(waiting, width);
  return fit(`${truncate(waiting.slice(0, -age.length), width - displayWidth(age))}${age}`, width);
}
