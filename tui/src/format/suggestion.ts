// How a Suggestion reads in the pane (design 2.6): its heading, and each
// change as "field  Jira's → yours".

import {
  type FieldValue,
  ItemField,
  type Item,
  type Patch,
  PatchState,
  Priority,
  ResolutionKind,
  StatusCategory,
  type Suggestion,
  SuggestionKind,
  SuggestionSource,
} from "@todo-tui/client";
import { dayOf, shortDay } from "./dates.js";
import { priorityName } from "./item.js";
import { singleLine, truncate } from "./text.js";

/** A status patch: the category, or the resolution kind; the two read and apply as one. */
export const statusPatch = (p: Patch): boolean => p.field === ItemField.STATUS_CATEGORY || p.field === ItemField.RESOLUTION_KIND;

/** "sync_conflict", "push_failed". */
export const kindWord = (s: Suggestion): string =>
  s.kind === SuggestionKind.SYNC_CONFLICT
    ? "sync_conflict"
    : s.kind === SuggestionKind.PUSH_FAILED
      ? "push_failed"
      : s.kind === SuggestionKind.REFINE
        ? "Claude"
        : "suggestion";

/** Claude's (ADR-0010): its rationale gets lines of its own, and its values are not Jira's. */
export const fromClaude = (s: Suggestion): boolean => s.source === SuggestionSource.CLAUDE;

/** The field a change sets, as a person names it. */
export function fieldWord(field: ItemField): string {
  switch (field) {
    case ItemField.TITLE:
      return "title";
    case ItemField.PRIORITY:
      return "priority";
    case ItemField.LABELS:
      return "labels";
    case ItemField.DUE_DATE:
      return "due";
    case ItemField.STATUS_CATEGORY:
    case ItemField.RESOLUTION_KIND:
      return "status";
    default:
      return "field";
  }
}

/** A patch value as text: "+2 high", "#a #b", "Fri Nov 13", "—" for none. */
export function valueText(v: FieldValue | undefined, today: number): string {
  const k = v?.kind;
  switch (k?.case) {
    case undefined:
    case "cleared":
      return "—";
    case "stringValue":
      return singleLine(k.value);
    case "priority":
      return k.value === Priority.NONE ? "—" : priorityName(k.value);
    case "stringSet":
      return k.value.values.length ? k.value.values.map((l) => `#${singleLine(l)}`).join(" ") : "—";
    case "dateValue":
      return shortDay(dayOf(k.value), today);
    default:
      return "?";
  }
}

/** A status pair as one word: to do, in progress, done, cancelled. */
function statusWord(category: FieldValue | undefined, kind: FieldValue | undefined, item: Item | undefined): string {
  const c = category?.kind.case === "statusCategory" ? category.kind.value : (item?.statusCategory ?? StatusCategory.TODO);
  if (c === StatusCategory.TODO) return "to do";
  if (c === StatusCategory.IN_PROGRESS) return "in progress";
  return kind?.kind.case === "resolutionKind" && kind.kind.value === ResolutionKind.CANCELLED ? "cancelled" : "done";
}

/** A change's text: "title  theirs (Jira) → mine (yours)". */
export function patchText(s: Suggestion, ids: readonly string[], item: Item | undefined, today: number): { field: string; from: string; to: string } {
  const patches = s.patches.filter((p) => ids.includes(p.id));
  const [first] = patches;
  if (!first) return { field: "?", from: "?", to: "?" };
  if (statusPatch(first)) {
    const cat = patches.find((p) => p.field === ItemField.STATUS_CATEGORY);
    const kind = patches.find((p) => p.field === ItemField.RESOLUTION_KIND);
    // A resolution kind alone is a change within done: done ↔ cancelled.
    const done = { kind: { case: "statusCategory" as const, value: StatusCategory.DONE } } as FieldValue;
    const catOld = cat?.expectedOld ?? (kind ? done : undefined);
    const catNew = cat?.new ?? (kind ? done : undefined);
    return { field: "status", from: statusWord(catOld, kind?.expectedOld, item), to: statusWord(catNew, kind?.new, item) };
  }
  return { field: fieldWord(first.field), from: valueText(first.expectedOld, today), to: valueText(first.new, today) };
}

/**
 * A heading: "PLAT-530 · sync_conflict · Jira changed the title on PLAT-530
 * while yours waited to push". Claude's names the Item and itself: its
 * rationale, often longer, goes on lines of its own under it.
 */
export function headingText(s: Suggestion, item: Item | undefined): string {
  const name = item ? (item.remoteLink?.key ?? truncate(singleLine(item.title), 40)) : "?";
  if (fromClaude(s)) return `${singleLine(name)} · ${kindWord(s)}`;
  return `${singleLine(name)} · ${kindWord(s)} · ${singleLine(s.rationale)}`;
}

/** Why a patch cannot be ticked, if it cannot. */
export const closedWord = (p: Patch): string | undefined =>
  p.state === PatchState.STALE ? "changed since" : p.state === PatchState.APPLIED ? "applied" : p.state === PatchState.REJECTED ? "rejected" : undefined;
