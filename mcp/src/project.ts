// What the tools return: a compact JSON shape of an Item for a model to read
// (design 4.6). Unset and empty fields are left out; remote fields appear
// once an Item is linked (P2), so nothing here changes when sync lands.

import {
  ArchiveReason,
  Energy,
  type FieldValue,
  type Item,
  ItemField,
  LinkState,
  Priority,
  type ProtoDate,
  ResolutionKind,
  Status,
  StatusCategory,
  ruleFromProto,
  ruleToken,
} from "@todo-tui/client/core";

/** The ULID's random tail, as `todo add` prints it (crates/todo-cli main.rs short_id). */
export function shortId(id: string): string {
  return id.slice(-8);
}

export const STATUS_NAMES: Record<string, Status> = {
  inbox: Status.INBOX,
  next: Status.NEXT,
  someday: Status.SOMEDAY,
  in_progress: Status.IN_PROGRESS,
  done: Status.DONE,
  cancelled: Status.CANCELLED,
};

const STATUS_OF = new Map(Object.entries(STATUS_NAMES).map(([name, s]) => [s, name]));

/** Capture's +1…+5 scale: 1 is highest. */
export const PRIORITY_LEVELS: Record<number, Priority> = {
  1: Priority.HIGHEST,
  2: Priority.HIGH,
  3: Priority.MEDIUM,
  4: Priority.LOW,
  5: Priority.LOWEST,
};

const LEVEL_OF = new Map(Object.entries(PRIORITY_LEVELS).map(([n, p]) => [p, Number(n)]));

/** Capture's %low / %medium / %high. */
export const ENERGY_LEVELS: Record<string, Energy> = { low: Energy.LOW, medium: Energy.MEDIUM, high: Energy.HIGH };

const ENERGY_OF = new Map(Object.entries(ENERGY_LEVELS).map(([name, e]) => [e, name]));

/** `YYYY-MM-DD`. */
export function isoDate(d: ProtoDate | undefined): string | undefined {
  if (d === undefined) return undefined;
  const pad = (n: number, w: number) => String(n).padStart(w, "0");
  return `${pad(d.year, 4)}-${pad(d.month, 2)}-${pad(d.day, 2)}`;
}

const date = isoDate;

function fieldName(f: ItemField): string {
  return (ItemField[f] ?? String(f)).toLowerCase();
}

/** Why sync archived an Item, as `archived` says it. */
const SYNC_ARCHIVE: ReadonlyMap<ArchiveReason, string> = new Map([
  [ArchiveReason.LEFT_SCOPE, "left_scope"],
  [ArchiveReason.REMOTE_UNAVAILABLE, "remote_unavailable"],
]);

export interface ProjectOptions {
  /** Include the description and private notes (todo_get). */
  detail?: boolean;
}

/** The Item as tool output; keys with no value are dropped. */
export function project(item: Item, opts: ProjectOptions = {}): Record<string, unknown> {
  const waiting = item.waiting;
  const on = waiting?.on?.ref;
  const rule = item.recurrence?.rule && ruleFromProto("rule", item.recurrence.rule);
  const link = item.remoteLink;
  const out: Record<string, unknown> = {
    id: item.id,
    short_id: shortId(item.id),
    title: item.title,
    status: STATUS_OF.get(item.status) ?? Status[item.status],
    priority: LEVEL_OF.get(item.effectivePriority),
    // With an override, priority above is the override and this is the item's own (Jira's, when linked).
    own_priority: item.localPriorityOverride !== undefined ? (LEVEL_OF.get(item.priority) ?? 0) : undefined,
    energy: ENERGY_OF.get(item.energy),
    due: date(item.dueDate),
    overdue: item.isOverdue || undefined,
    defer_until: date(item.deferUntil),
    labels: item.labels.length > 0 ? item.labels : undefined,
    waiting: waiting && {
      on: on?.case === "freeText" ? on.value : on?.value,
      reason: waiting.reason || undefined,
      since: date(waiting.since),
      follow_up_on: date(waiting.followUpOn),
    },
    repeat: rule && "rule" in rule ? ruleToken(rule.rule) : undefined,
    parent: item.parentTitle,
    subtasks: item.subtasksTotal > 0 ? `${item.subtasksDone}/${item.subtasksTotal}` : undefined,
    // Sync's reasons say why; a person's archive is just true.
    archived: item.archived ? (SYNC_ARCHIVE.get(item.archived.reason) ?? true) : undefined,
    deleted: item.deletedAt ? true : undefined,
    jira: link && {
      key: link.key,
      url: link.url,
      instance: link.instanceId,
      tracker: link.trackerKind,
      remote_status: item.remoteStatusName,
      unavailable: link.linkState === LinkState.BROKEN || undefined,
    },
    has_conflict: item.hasConflict || undefined,
    push_failed: item.hasPushFailure || undefined,
    pending_fields: item.pendingFields.length > 0 ? item.pendingFields.map(fieldName) : undefined,
  };
  if (opts.detail) {
    out.description = item.description || undefined;
    out.private_notes = item.privateNotes || undefined;
    out.created_at = item.createdAt && new Date(Number(item.createdAt.seconds) * 1000).toISOString();
  }
  return prune(out);
}

/** A Suggestion patch's value as plain JSON: a string, number, list, date, or null for none. */
export function valueOf(v: FieldValue | undefined): unknown {
  const k = v?.kind;
  if (!k) return null;
  switch (k.case) {
    case undefined:
    case "cleared":
      return null;
    case "stringSet":
      return [...k.value.values];
    case "dateValue":
      return isoDate(k.value) ?? null;
    case "priority":
      return LEVEL_OF.get(k.value) ?? 0;
    case "statusCategory":
      return StatusCategory[k.value]?.toLowerCase() ?? null;
    case "resolutionKind":
      return ResolutionKind[k.value]?.toLowerCase() ?? null;
    case "stringValue":
      return k.value;
    default:
      return String(k.value);
  }
}

export function prune(o: Record<string, unknown>): Record<string, unknown> {
  for (const [k, v] of Object.entries(o)) {
    if (v === undefined || v === "") delete o[k];
    else if (v !== null && typeof v === "object" && !Array.isArray(v)) {
      prune(v as Record<string, unknown>);
      if (Object.keys(v).length === 0) delete o[k];
    }
  }
  return o;
}
