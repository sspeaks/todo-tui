// How an Item reads in a list row and in the detail pane (design 2.2).

import { CreatedVia, Energy, type Item, type Person, type PersonRef, Priority, Status } from "@todo-tui/client/portable";
import { dayOf, shortDay } from "./dates.js";
import { singleLine } from "./text.js";

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

const energyGlyphs: Partial<Record<Energy, string>> = {
  [Energy.LOW]: "▁",
  [Energy.MEDIUM]: "▄",
  [Energy.HIGH]: "█",
};

const energyNames: Partial<Record<Energy, string>> = {
  [Energy.LOW]: "low",
  [Energy.MEDIUM]: "medium",
  [Energy.HIGH]: "high",
};

const createdVia: Partial<Record<CreatedVia, string>> = {
  [CreatedVia.TUI]: "tui",
  [CreatedVia.CLI]: "cli",
  [CreatedVia.MCP]: "mcp",
  [CreatedVia.REMOTE_IMPORT]: "import",
  [CreatedVia.RESTORE]: "restore",
  [CreatedVia.SPLIT]: "split",
  [CreatedVia.RECURRENCE]: "recurrence",
  [CreatedVia.MOBILE]: "mobile",
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

/** The one-cell energy glyph of the row (design 2.2): ▁ low, ▄ medium, █ high; "" for none. */
export function energyGlyph(energy: Energy): string {
  return energyGlyphs[energy] ?? "";
}

/** "low", "medium", "high", or "none". */
export const energyName = (e: Energy): string => energyNames[e] ?? "none";

/** "+3 medium", or "none". */
export const priorityName = (p: Priority): string => (priorities[p] ? `${priorities[p]} ${priorityNames[p]}` : "none");

/** The People a client knows, by id (LiveStore.people). */
export interface People {
  get(id: string): Person | undefined;
}

/**
 * A person as text: the free text, or the Person's display name (design
 * 3.5); "someone" for a person not known yet (never their id).
 */
export function personName(p: PersonRef | undefined, people?: People): string {
  if (p?.ref.case === "freeText") return singleLine(p.ref.value);
  if (p?.ref.case === "personId") {
    const name = people?.get(p.ref.value)?.displayName;
    return name ? singleLine(name) : "someone";
  }
  return "";
}

/** A person's first email address, or "". */
export function personEmail(p: PersonRef | undefined, people?: People): string {
  if (p?.ref.case !== "personId") return "";
  return singleLine(people?.get(p.ref.value)?.emails[0] ?? "");
}

/** The short id the CLI prints: the last 8 characters of the ULID. */
export const shortId = (id: string): string => id.slice(-8);

/** The cells of a list row after the title; empty when unset. */
export interface RowCells {
  /** The parent's title, shown dim after the title in the room it leaves. */
  parent: string;
  /** "2/5": done of the subtasks not deleted or cancelled, on a parent. */
  progress: string;
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
  /** The energy glyph (▁ ▄ █) after the priority; "" for none. */
  energy: string;
}

export function rowCells(item: Item, today: number, people?: People): RowCells {
  const waiting = item.waiting;
  let waitingText = "";
  let age = "";
  if (waiting) {
    age = waiting.since ? ` ${Math.max(0, today - dayOf(waiting.since))}d` : "";
    waitingText = `⧗${personName(waiting.on, people)}${age}`;
  }
  const markers: string[] = [];
  if (item.deferUntil && dayOf(item.deferUntil) > today) markers.push(`⏸${shortDay(dayOf(item.deferUntil), today)}`);
  if (item.pendingFields.length) markers.push("↑");
  if (item.hasConflict || item.hasPushFailure) markers.push("!");
  if (item.recurrence) markers.push("🔁");
  return {
    parent: singleLine(item.parentTitle ?? ""),
    progress: item.subtasksTotal ? `${item.subtasksDone}/${item.subtasksTotal}` : "",
    key: singleLine(item.remoteLink?.key ?? ""),
    due: item.dueDate ? `^${shortDay(dayOf(item.dueDate), today)}` : "",
    overdue: item.isOverdue,
    waiting: waitingText,
    waitingAge: age,
    followUpDue: waiting?.followUpOn !== undefined && dayOf(waiting.followUpOn) <= today,
    markers: markers.join(" "),
    energy: energyGlyph(item.energy),
  };
}
