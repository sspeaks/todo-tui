// The filter language (filter.proto, design 4.8) for FakeDaemon: the checks
// with their error paths, relative dates, the evaluator, the sort order,
// keyset cursors and an approximate MATCHES. Ported from
// crates/todo-core/src/filter.rs, the reference semantics the daemon's SQL
// agrees with, and from the filter conversion in
// crates/todo-proto/src/convert.rs, which decides what an operand that does
// not read is.

import { clone, create, toBinary } from "@bufbuild/protobuf";
import type { Duration, Timestamp } from "@bufbuild/protobuf/wkt";
import type { PersonRef } from "./gen/todo/v1/common_pb.js";
import { ItemField } from "./gen/todo/v1/field_pb.js";
import type { FieldValue } from "./gen/todo/v1/field_value_pb.js";
import {
  type Clause,
  ClauseSchema,
  type Filter,
  FilterSchema,
  type Operand,
  Operator,
  RelativeDate_Anchor as Anchor,
  type SortKey,
  SortKeySchema,
} from "./gen/todo/v1/filter_pb.js";
import { ArchiveReason, CreatedVia, type Item, LinkState, Priority, ResolutionKind, Status, StatusCategory, TodoRefinement } from "./gen/todo/v1/item_pb.js";
import type { View } from "./gen/todo/v1/view_pb.js";
import { dayNumber, dayOf, isDate, isIsoWeek, isoWeek, weekday } from "./dates.js";
import { MAX_GROUP_DEPTH, MAX_MATCHES_BYTES, MAX_OPERANDS, MAX_PREDICATE_OPERANDS, MAX_PREDICATES, MAX_SORT_KEYS } from "./limits.js";

/** Why a filter or sort was refused: INVALID_ARGUMENT, or UNIMPLEMENTED for a valid one not built yet. */
export interface FilterProblem {
  readonly kind: "invalid" | "unimplemented";
  /** The request path, e.g. "filter.all_of[1].group.any_of[0].predicate.op" or "sort[2].key". */
  readonly path: string;
  readonly reason: string;
}

// ---- the target catalogue -----------------------------------------------------

/** What a target holds, which decides the operators and operands it takes. */
type ValueClass =
  | "enum"
  | "priority"
  | "string"
  | "id"
  | "markdown"
  | "date"
  | "focus_week"
  | "instant"
  | "number"
  | "duration"
  | "uint"
  | "bool"
  | "set"
  | "person"
  | "presence"
  | "pending_fields"
  | "remote_extras";

interface Target {
  readonly class: ValueClass;
  /** Whether it can be unset, so that IS_SET and IS_UNSET apply (sets and pending_fields take them as non-empty and empty). */
  readonly canUnset: boolean;
  readonly sortable: boolean;
  /** Whether MATCHES searches it. */
  readonly matches: boolean;
}

const F = ItemField;

const CATALOGUE = new Map<ItemField, Target>();

function add(cls: ValueClass, canUnset: boolean, sortable: boolean, ...fields: ItemField[]): void {
  const searched = [F.TITLE, F.DESCRIPTION, F.LABELS, F.REMOTE_LINK_KEY];
  for (const f of fields) CATALOGUE.set(f, { class: cls, canUnset, sortable, matches: searched.includes(f) });
}

add("id", false, true, F.ID);
add("id", true, false, F.PARENT_ID, F.MERGED_INTO);
add("enum", false, true, F.CREATED_VIA, F.STATUS_CATEGORY, F.TODO_REFINEMENT, F.STATUS);
add("enum", true, true, F.RESOLUTION_KIND, F.ARCHIVED_REASON, F.REMOTE_LINK_LINK_STATE);
add("instant", false, true, F.CREATED_AT, F.UPDATED_AT);
add(
  "instant",
  true,
  true,
  F.REMOTE_CREATED_AT,
  F.REMOTE_UPDATED_AT,
  F.DELETED_AT,
  F.ARCHIVED_AT,
  F.REMOTE_LINK_LAST_PULLED_VERSION,
  F.STALENESS_LAST_USER_TOUCH_AT,
  F.STALENESS_LAST_REMOTE_CHANGE_AT,
  F.STALENESS_STATUS_ENTERED_AT,
  F.STALENESS_WAITING_SINCE,
);
add("uint", false, true, F.REVISION);
add("string", false, true, F.TITLE, F.ITEM_TYPE);
add(
  "string",
  true,
  true,
  F.REMOTE_STATUS_NAME,
  F.SPRINT,
  F.RESOLUTION,
  F.WAITING_REASON,
  F.REMOTE_LINK_TRACKER_KIND,
  F.REMOTE_LINK_INSTANCE_ID,
  F.REMOTE_LINK_REMOTE_ID,
  F.REMOTE_LINK_KEY,
  F.REMOTE_LINK_URL,
);
// A ULID as text, which has no useful order.
add("string", true, false, F.RECURRENCE_SERIES_ID);
add("markdown", false, false, F.DESCRIPTION, F.PRIVATE_NOTES);
add("priority", false, true, F.PRIORITY, F.EFFECTIVE_PRIORITY);
add("priority", true, true, F.LOCAL_PRIORITY_OVERRIDE);
add("person", true, false, F.ASSIGNEE, F.REPORTER, F.WAITING_ON);
add("set", false, false, F.LABELS, F.COMPONENTS, F.FIX_VERSIONS, F.SCOPES);
add("date", true, true, F.DUE_DATE, F.DEFER_UNTIL, F.WAITING_SINCE, F.WAITING_FOLLOW_UP_ON, F.RECURRENCE_OCCURRENCE_DATE);
add("focus_week", true, true, F.FOCUS_WEEK);
add("number", true, true, F.STORY_POINTS);
add("duration", true, true, F.ORIGINAL_ESTIMATE, F.REMAINING_ESTIMATE);
add("bool", false, true, F.IS_MINE, F.HAS_CONFLICT, F.HAS_PUSH_FAILURE, F.DESCRIPTION_LOSSY, F.DESCRIPTION_DIRTY, F.IS_OVERDUE);
add("bool", true, true, F.REMOTE_LINK_PINNED);
add("presence", true, false, F.REMOTE_LINK, F.WAITING, F.ARCHIVED, F.RECURRENCE, F.STALENESS);
add("pending_fields", false, false, F.PENDING_FIELDS);
add("remote_extras", false, false, F.REMOTE_EXTRAS);

const ORDERED = new Set<ValueClass>(["priority", "date", "focus_week", "instant", "number", "duration", "uint"]);

type ValueCase = NonNullable<FieldValue["kind"]["case"]>;

/** The enums a value can hold, by FieldValue case: valid numbers are known and not UNSPECIFIED. */
const ENUMS: Partial<Record<ValueCase, object>> = {
  priority: Priority,
  status: Status,
  statusCategory: StatusCategory,
  resolutionKind: ResolutionKind,
  todoRefinement: TodoRefinement,
  createdVia: CreatedVia,
  archiveReason: ArchiveReason,
  linkState: LinkState,
};

const ENUM_CASES = new Map<ItemField, ValueCase>([
  [F.CREATED_VIA, "createdVia"],
  [F.STATUS_CATEGORY, "statusCategory"],
  [F.RESOLUTION_KIND, "resolutionKind"],
  [F.TODO_REFINEMENT, "todoRefinement"],
  [F.STATUS, "status"],
  [F.ARCHIVED_REASON, "archiveReason"],
  [F.REMOTE_LINK_LINK_STATE, "linkState"],
]);

/** The FieldValue case a value operand on `field` has (a string per member for a set); undefined for targets that take no values. */
function valueCase(field: ItemField): ValueCase | undefined {
  switch (CATALOGUE.get(field)?.class) {
    case "enum":
      return ENUM_CASES.get(field);
    case "id":
      return "itemId";
    case "instant":
      return "timestampValue";
    case "uint":
      return "uintValue";
    case "string":
    case "set":
      return "stringValue";
    case "markdown":
      return "markdownValue";
    case "priority":
      return "priority";
    case "person":
      return "person";
    case "date":
      return "dateValue";
    case "focus_week":
      return "isoWeek";
    case "number":
      return "numberValue";
    case "duration":
      return "durationValue";
    case "bool":
      return "boolValue";
    default:
      return undefined;
  }
}

/** A FieldValue case as core names its variant: "string", "item_id", "archive_reason". */
function kindName(c: string): string {
  return c.replace(/Value$/, "").replace(/[A-Z]/g, (ch) => `_${ch.toLowerCase()}`);
}

const OPERATOR_NAMES: Record<number, string> = {
  [Operator.IN]: "IN",
  [Operator.EQ]: "EQ",
  [Operator.NE]: "NE",
  [Operator.LT]: "LT",
  [Operator.LE]: "LE",
  [Operator.GT]: "GT",
  [Operator.GE]: "GE",
  [Operator.IS_SET]: "IS_SET",
  [Operator.IS_UNSET]: "IS_UNSET",
  [Operator.CONTAINS_ANY]: "CONTAINS_ANY",
  [Operator.CONTAINS_ALL]: "CONTAINS_ALL",
  [Operator.MATCHES]: "MATCHES",
};

const isOrdering = (op: Operator) => op === Operator.LT || op === Operator.LE || op === Operator.GT || op === Operator.GE;
const isContains = (op: Operator) => op === Operator.CONTAINS_ANY || op === Operator.CONTAINS_ALL;

// ---- checks -------------------------------------------------------------------

/** The first INVALID found, thrown to end the walk. */
class Invalid {
  constructor(
    readonly path: string,
    readonly reason: string,
  ) {}
}

interface Walk {
  /** With a today, relative dates are resolved too, so one out of range is reported in its place. */
  readonly today: number | undefined;
  predicates: number;
  operands: number;
  unimplemented: FilterProblem | undefined;
}

/** An operand as it reads, before its fit to the target. */
type ReadOperand =
  | { kind: "value"; value: FieldValue }
  | { kind: "relative"; anchor: Anchor; offsetDays: number }
  | { kind: "text"; text: string };

/**
 * Checks a request's filter as the daemon reads it (filter.proto): the first
 * INVALID in document order (all_of before any_of, each clause depth-first;
 * in a predicate its target, op, operand count, then each operand, its form
 * before its fit), else a total over the limits (on "filter"), else the
 * first UNIMPLEMENTED. Paths start with "filter". With `today`, a relative
 * date out of range is reported in its place.
 */
export function checkFilter(filter: Filter, today?: number): FilterProblem | undefined {
  const walk: Walk = { today, predicates: 0, operands: 0, unimplemented: undefined };
  try {
    checkClauses(walk, "filter", filter.allOf, filter.anyOf, 0);
  } catch (e) {
    if (e instanceof Invalid) return { kind: "invalid", path: e.path, reason: e.reason };
    throw e;
  }
  if (walk.predicates > MAX_PREDICATES) {
    return { kind: "invalid", path: "filter", reason: `holds ${walk.predicates} predicates; at most ${MAX_PREDICATES}` };
  }
  if (walk.operands > MAX_OPERANDS) {
    return { kind: "invalid", path: "filter", reason: `holds ${walk.operands} operands; at most ${MAX_OPERANDS}` };
  }
  return walk.unimplemented;
}

function checkClauses(walk: Walk, path: string, allOf: readonly Clause[], anyOf: readonly Clause[], depth: number): void {
  for (const [list, clauses] of [
    ["all_of", allOf],
    ["any_of", anyOf],
  ] as const) {
    for (const [i, c] of clauses.entries()) {
      const at = `${path}.${list}[${i}]`;
      switch (c.kind.case) {
        case undefined:
          throw new Invalid(at, "must be a predicate or a group");
        case "predicate": {
          const p = `${at}.predicate`;
          const { target, op, operands } = c.kind.value;
          if (target === F.UNSPECIFIED) throw new Invalid(`${p}.target`, "must not be UNSPECIFIED");
          if (!CATALOGUE.has(target)) throw new Invalid(`${p}.target`, `unknown enum value ${target}`);
          if (op === Operator.UNSPECIFIED) throw new Invalid(`${p}.op`, "must not be UNSPECIFIED");
          if (OPERATOR_NAMES[op] === undefined) throw new Invalid(`${p}.op`, `unknown enum value ${op}`);
          checkPredicate(walk, p, target, op, operands.length);
          for (const [k, o] of operands.entries()) {
            const where = `${p}.operands[${k}]`;
            checkOperand(walk, where, target, op, readOperand(where, o));
          }
          break;
        }
        case "group": {
          const g = `${at}.group`;
          if (depth + 1 > MAX_GROUP_DEPTH) throw new Invalid(g, `groups nest at most ${MAX_GROUP_DEPTH} deep`);
          checkClauses(walk, g, c.kind.value.allOf, c.kind.value.anyOf, depth + 1);
          break;
        }
      }
    }
  }
}

function notBuilt(walk: Walk, path: string, reason: string): void {
  walk.unimplemented ??= { kind: "unimplemented", path, reason };
}

/** A predicate at `p` on `field` with `op` and `count` operands. */
function checkPredicate(walk: Walk, p: string, field: ItemField, op: Operator, count: number): void {
  walk.predicates++;
  walk.operands += count;
  const t = CATALOGUE.get(field)!;
  const name = OPERATOR_NAMES[op]!;
  const opError = (reason: string) => new Invalid(`${p}.op`, reason);
  if (op === Operator.MATCHES) {
    if (!t.matches) throw new Invalid(`${p}.target`, "MATCHES searches only title, description, labels and remote_link.key");
  } else if (t.class === "remote_extras") {
    notBuilt(walk, `${p}.target`, "filters on remote_extras are not supported yet");
  } else if (op === Operator.IS_SET || op === Operator.IS_UNSET) {
    if (!t.canUnset && t.class !== "set" && t.class !== "pending_fields") throw opError(`${name} applies only to a target that can be unset; use EQ`);
  } else if (isContains(op)) {
    if (t.class === "pending_fields") notBuilt(walk, `${p}.op`, `${name} on pending_fields is not supported yet`);
    else if (t.class !== "set") throw opError(`${name} applies only to set targets`);
  } else if (op === Operator.IN || op === Operator.EQ || op === Operator.NE) {
    if (t.class === "set") throw opError(`${name} does not apply to set targets; use CONTAINS_ANY or CONTAINS_ALL`);
    if (t.class === "presence" || t.class === "pending_fields") throw opError(`${name} does not apply to ${t.class} targets; use IS_SET or IS_UNSET`);
  } else if (!ORDERED.has(t.class)) {
    throw opError(`${name} does not apply to ${t.class} targets, which have no order`);
  }
  const many = op === Operator.IN || isContains(op);
  const none = op === Operator.IS_SET || op === Operator.IS_UNSET;
  const fits = many ? count >= 1 && count <= MAX_PREDICATE_OPERANDS : none ? count === 0 : count === 1;
  if (!fits) {
    const reason = many ? `${name} takes 1 to ${MAX_PREDICATE_OPERANDS} operands` : none ? `${name} takes no operands` : `${name} takes exactly one operand`;
    throw new Invalid(`${p}.operands`, reason);
  }
}

/** The operand at `path` as it reads: what does not read is named as the whole operand (an anchor on its own). */
function readOperand(path: string, o: Operand): ReadOperand {
  switch (o.kind.case) {
    case undefined:
      throw new Invalid(path, "must be a value, a relative date or text");
    case "value": {
      const problem = unreadable(o.kind.value);
      if (problem) throw new Invalid(path, problem);
      return { kind: "value", value: o.kind.value };
    }
    case "relativeDate": {
      const { anchor, offsetDays } = o.kind.value;
      if (anchor === Anchor.UNSPECIFIED) throw new Invalid(`${path}.relative_date.anchor`, "must not be UNSPECIFIED");
      if (!(anchor in Anchor)) throw new Invalid(`${path}.relative_date.anchor`, `unknown enum value ${anchor}`);
      return { kind: "relative", anchor, offsetDays };
    }
    case "text":
      return { kind: "text", text: o.kind.value };
  }
}

const NANOS_PER_SEC = 1_000_000_000n;
/** Proto3's limit on Duration.seconds (about 10,000 years). */
const MAX_DURATION_SECS = 315_576_000_000n;
/** 0001-01-01T00:00:00Z, the earliest Timestamp. */
const MIN_TIMESTAMP_SECS = -62_135_596_800n;
const I64_MIN = -(2n ** 63n);
const I64_MAX = 2n ** 63n - 1n;

const ULID = /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/i;

/** Why a FieldValue does not read as the daemon converts it (field_value_from_proto), if it does not. */
function unreadable(v: FieldValue): string | undefined {
  const k = v.kind;
  const known = (e: object, n: number) => n !== 0 && typeof (e as Record<number, unknown>)[n] === "string";
  switch (k.case) {
    case undefined:
      return "value.kind: required field is missing";
    case "dateValue": {
      const { year, month, day } = k.value;
      return year >= 1 && year <= 9999 && isDate(year, month, day) ? undefined : "value.date_value: not a date in years 1 to 9999";
    }
    case "timestampValue":
      if (k.value.nanos < 0 || k.value.nanos >= 1e9) return "value.timestamp_value: nanos out of range";
      return k.value.seconds < MIN_TIMESTAMP_SECS ? "value.timestamp_value: before 0001-01-01T00:00:00Z" : undefined;
    case "durationValue": {
      const { seconds, nanos } = k.value;
      if (Math.abs(nanos) >= 1e9) return "value.duration_value: nanos out of range";
      if (seconds > MAX_DURATION_SECS || seconds < -MAX_DURATION_SECS) return "value.duration_value: seconds out of range";
      return seconds !== 0n && nanos !== 0 && seconds < 0n !== nanos < 0 ? "value.duration_value: seconds and nanos differ in sign" : undefined;
    }
    case "person":
      if (k.value.ref.case === undefined) return "value.person: required field is missing";
      return k.value.ref.case === "personId" && !ULID.test(k.value.ref.value) ? "value.person: not a ULID" : undefined;
    case "itemId":
      return ULID.test(k.value) ? undefined : "value.item_id: not a ULID";
    case "isoWeek":
      return isIsoWeek(k.value) ? undefined : "value.iso_week: not an ISO week (YYYY-Www)";
    case "itemFieldSet":
      return k.value.fields.every((f) => CATALOGUE.has(f)) ? undefined : "value.item_field_set.fields: unknown field";
    default: {
      const e = ENUMS[k.case];
      return e && !known(e, k.value as number) ? `value.${kindName(k.case)}: not a known value` : undefined;
    }
  }
}

/** An operand at `path` of a predicate on `field` with `op`, which checkPredicate accepted. */
function checkOperand(walk: Walk, path: string, field: ItemField, op: Operator, operand: ReadOperand): void {
  const cls = CATALOGUE.get(field)!.class;
  // No target takes these, built or not.
  if (operand.kind === "text" && op !== Operator.MATCHES) throw new Invalid(path, "text is only for MATCHES");
  if (operand.kind === "value" && operand.value.kind.case === "cleared") throw new Invalid(path, "cleared is never an operand; use IS_UNSET");
  // Nothing more to fit an operand to until these are built.
  if (cls === "remote_extras" || (cls === "pending_fields" && isContains(op))) return;
  if (operand.kind === "text") {
    if (/^[\x00-\x20]*$/.test(operand.text)) throw new Invalid(path, "must not be blank");
    if (new TextEncoder().encode(operand.text).length > MAX_MATCHES_BYTES) throw new Invalid(path, `is longer than ${MAX_MATCHES_BYTES} bytes`);
    return;
  }
  if (op === Operator.MATCHES) throw new Invalid(path, "MATCHES takes a text operand");
  if (operand.kind === "relative") {
    const { anchor, offsetDays } = operand;
    const week = anchor === Anchor.CURRENT_WEEK;
    if (cls === "instant" && !week) {
      notBuilt(walk, path, "relative dates on instants are not supported yet");
      return;
    }
    if (!((cls === "date" && !week) || (cls === "focus_week" && week))) {
      const reason =
        cls === "date" || cls === "instant"
          ? "CURRENT_WEEK is a week, for focus_week"
          : cls === "focus_week"
            ? "focus_week takes CURRENT_WEEK"
            : "relative dates are only for dates and focus_week";
      throw new Invalid(path, reason);
    }
    const offset = `${path}.relative_date.offset_days`;
    if (week && offsetDays !== 0) throw new Invalid(offset, "must be 0 for CURRENT_WEEK");
    if (walk.today !== undefined && resolveRelative(anchor, offsetDays, walk.today) === undefined) {
      throw new Invalid(offset, "names a date outside 0001-01-01 to 9999-12-31");
    }
    return;
  }
  const problem = valueProblem(field, op, operand.value);
  if (problem) throw new Invalid(path, problem);
}

/** What is wrong with `v` (read, not cleared) as an operand of `op` on `field`, if anything. */
function valueProblem(field: ItemField, op: Operator, v: FieldValue): string | undefined {
  const want = valueCase(field);
  if (want === undefined) return "takes no values";
  const k = v.kind;
  if (k.case !== want) return `takes ${kindName(want)} values, not ${kindName(k.case ?? "")}`;
  switch (k.case) {
    case "priority":
      return k.value === Priority.NONE && isOrdering(op) ? "PRIORITY_NONE is outside the priority order" : undefined;
    case "timestampValue":
      return inI64(nanosOf(k.value)) ? undefined : "must be between 1677-09-21T00:12:44Z and 2262-04-11T23:47:16Z";
    case "durationValue":
      return inI64(nanosOf(k.value)) ? undefined : "must be shorter than about 292 years";
    case "numberValue":
      return Number.isFinite(k.value) ? undefined : "must be a finite number";
    case "uintValue":
      return k.value > I64_MAX ? "must be at most 2^63 - 1" : undefined;
    case "person":
      return k.value.ref.case === "freeText" && field !== F.WAITING_ON ? "free text is only valid for waiting.on" : undefined;
    default:
      return undefined;
  }
}

function nanosOf(t: Timestamp | Duration): bigint {
  return t.seconds * NANOS_PER_SEC + BigInt(t.nanos);
}

function inI64(n: bigint): boolean {
  return n >= I64_MIN && n <= I64_MAX;
}

/**
 * Checks a sort (filter.proto SortKey): each key known, sortable and given
 * once, then at most MAX_SORT_KEYS keys. Paths are "sort[i].key" and "sort".
 */
export function checkSort(keys: readonly SortKey[]): FilterProblem | undefined {
  const invalid = (path: string, reason: string): FilterProblem => ({ kind: "invalid", path, reason });
  for (const [i, k] of keys.entries()) {
    const path = `sort[${i}].key`;
    if (k.key === F.UNSPECIFIED) return invalid(path, "must not be UNSPECIFIED");
    const t = CATALOGUE.get(k.key);
    if (!t) return invalid(path, `unknown enum value ${k.key}`);
    if (!t.sortable) {
      const reference = t.class === "id" || t.class === "string";
      return invalid(path, reference ? "is not a sort key: a reference to another record has no useful order" : `is not a sort key: ${t.class} targets have no order`);
    }
    const j = keys.slice(0, i).findIndex((x) => x.key === k.key);
    if (j >= 0) return invalid(path, `repeats sort[${j}].key`);
  }
  if (keys.length > MAX_SORT_KEYS) return invalid("sort", `holds ${keys.length} keys; at most ${MAX_SORT_KEYS}`);
  return undefined;
}

// ---- values -------------------------------------------------------------------

/**
 * A target's value on an Item, or an operand's, as the evaluator compares
 * them: strings (also Markdown, ids and ISO weeks) by code point, instants,
 * durations and uints as BigInt nanoseconds or counts, dates as day numbers,
 * enums by proto number, priorities by rank.
 */
export type Value =
  | { readonly k: "str"; readonly v: string }
  | { readonly k: "num"; readonly v: number }
  | { readonly k: "big"; readonly v: bigint }
  | { readonly k: "day"; readonly v: number }
  | { readonly k: "bool"; readonly v: boolean }
  | { readonly k: "enum"; readonly v: number }
  | { readonly k: "prio"; readonly v: Priority }
  | { readonly k: "set"; readonly v: readonly string[] }
  | { readonly k: "person"; readonly id?: string; readonly text?: string }
  | { readonly k: "fields"; readonly v: readonly number[] }
  | { readonly k: "present" };

const str = (v: string | undefined): Value | undefined => (v === undefined ? undefined : { k: "str", v });
const big = (t: Timestamp | Duration | undefined): Value | undefined => (t === undefined ? undefined : { k: "big", v: nanosOf(t) });
const day = (d: Item["dueDate"]): Value | undefined => (d === undefined ? undefined : { k: "day", v: dayOf(d) });
const bool = (v: boolean | undefined): Value | undefined => (v === undefined ? undefined : { k: "bool", v });
const enumOf = (v: number | undefined): Value | undefined => (v === undefined ? undefined : { k: "enum", v });
const present = (m: object | undefined): Value | undefined => (m === undefined ? undefined : { k: "present" });

/** A stored priority; an Item put without one (UNSPECIFIED) has none. */
const prio = (v: Priority | undefined): Value | undefined => (v === undefined ? undefined : { k: "prio", v: v || Priority.NONE });

function person(p: PersonRef | undefined): Value | undefined {
  switch (p?.ref.case) {
    case "personId":
      return { k: "person", id: p.ref.value.toUpperCase() };
    case "freeText":
      return { k: "person", text: p.ref.value };
    default:
      return undefined;
  }
}

/** Due before `today` and not done or cancelled; archive, defer and delete do not matter. */
export function isOverdue(item: Item, today: number): boolean {
  return item.dueDate !== undefined && dayOf(item.dueDate) < today && item.status !== Status.DONE && item.status !== Status.CANCELLED;
}

/** The priority that orders an Item: its override if set. */
export function effectivePriority(item: Item): Priority {
  return item.localPriorityOverride ?? (item.priority || Priority.NONE);
}

/** `field` on `item` on `today`; undefined when unset (for a nested field, also when its message is). */
export function valueOf(item: Item, field: ItemField, today: number): Value | undefined {
  const { waiting, archived, remoteLink: link, recurrence, staleness } = item;
  switch (field) {
    case F.ID:
      return { k: "str", v: item.id };
    case F.CREATED_VIA:
      return enumOf(item.createdVia);
    case F.CREATED_AT:
      return big(item.createdAt);
    case F.UPDATED_AT:
      return big(item.updatedAt);
    case F.REVISION:
      return { k: "big", v: item.revision };
    case F.REMOTE_LINK:
      return present(link);
    case F.TITLE:
      return str(item.title);
    case F.DESCRIPTION:
      return str(item.description);
    case F.ITEM_TYPE:
      return str(item.itemType);
    case F.STATUS_CATEGORY:
      return enumOf(item.statusCategory);
    case F.RESOLUTION_KIND:
      return enumOf(item.resolutionKind);
    case F.REMOTE_STATUS_NAME:
      return str(item.remoteStatusName);
    case F.PRIORITY:
      return prio(item.priority);
    case F.ASSIGNEE:
      return person(item.assignee);
    case F.REPORTER:
      return person(item.reporter);
    case F.LABELS:
      return { k: "set", v: item.labels };
    case F.COMPONENTS:
      return { k: "set", v: item.components };
    case F.FIX_VERSIONS:
      return { k: "set", v: item.fixVersions };
    case F.DUE_DATE:
      return day(item.dueDate);
    case F.PARENT_ID:
      return str(item.parentId?.toUpperCase());
    case F.SPRINT:
      return str(item.sprint);
    case F.STORY_POINTS:
      return item.storyPoints === undefined ? undefined : { k: "num", v: item.storyPoints };
    case F.ORIGINAL_ESTIMATE:
      return big(item.originalEstimate);
    case F.REMAINING_ESTIMATE:
      return big(item.remainingEstimate);
    case F.RESOLUTION:
      return str(item.resolution);
    case F.REMOTE_CREATED_AT:
      return big(item.remoteCreatedAt);
    case F.REMOTE_UPDATED_AT:
      return big(item.remoteUpdatedAt);
    case F.TODO_REFINEMENT:
      return enumOf(item.todoRefinement);
    case F.WAITING:
      return present(waiting);
    case F.DEFER_UNTIL:
      return day(item.deferUntil);
    case F.PRIVATE_NOTES:
      return str(item.privateNotes);
    case F.FOCUS_WEEK:
      return str(item.focusWeek);
    case F.LOCAL_PRIORITY_OVERRIDE:
      return prio(item.localPriorityOverride);
    case F.ARCHIVED:
      return present(archived);
    case F.RECURRENCE:
      return present(recurrence);
    case F.STATUS:
      return enumOf(item.status);
    case F.EFFECTIVE_PRIORITY:
      return prio(effectivePriority(item));
    case F.IS_MINE:
      return bool(item.isMine);
    case F.PENDING_FIELDS:
      return { k: "fields", v: item.pendingFields };
    case F.HAS_CONFLICT:
      return bool(item.hasConflict);
    case F.HAS_PUSH_FAILURE:
      return bool(item.hasPushFailure);
    case F.DESCRIPTION_LOSSY:
      return bool(item.descriptionLossy);
    case F.DESCRIPTION_DIRTY:
      return bool(item.descriptionDirty);
    case F.STALENESS: {
      const s = staleness;
      return present(s && (s.lastUserTouchAt ?? s.lastRemoteChangeAt ?? s.statusEnteredAt ?? s.waitingSince) ? s : undefined);
    }
    case F.SCOPES:
      return { k: "set", v: item.scopes };
    case F.DELETED_AT:
      return big(item.deletedAt);
    case F.MERGED_INTO:
      return str(item.mergedInto?.toUpperCase());
    case F.IS_OVERDUE:
      return bool(isOverdue(item, today));
    case F.WAITING_ON:
      return person(waiting?.on);
    case F.WAITING_REASON:
      return str(waiting?.reason);
    case F.WAITING_SINCE:
      return day(waiting?.since);
    case F.WAITING_FOLLOW_UP_ON:
      return day(waiting?.followUpOn);
    case F.ARCHIVED_AT:
      return big(archived?.at);
    case F.ARCHIVED_REASON:
      return enumOf(archived?.reason);
    case F.REMOTE_LINK_TRACKER_KIND:
      return str(link?.trackerKind);
    case F.REMOTE_LINK_INSTANCE_ID:
      return str(link?.instanceId);
    case F.REMOTE_LINK_REMOTE_ID:
      return str(link?.remoteId);
    case F.REMOTE_LINK_KEY:
      return str(link?.key);
    case F.REMOTE_LINK_URL:
      return str(link?.url);
    case F.REMOTE_LINK_LAST_PULLED_VERSION:
      return big(link?.lastPulledVersion);
    case F.REMOTE_LINK_LINK_STATE:
      return enumOf(link?.linkState);
    case F.REMOTE_LINK_PINNED:
      return bool(link?.pinned);
    case F.RECURRENCE_SERIES_ID:
      return str(recurrence?.seriesId);
    case F.RECURRENCE_OCCURRENCE_DATE:
      return day(recurrence?.occurrenceDate);
    case F.STALENESS_LAST_USER_TOUCH_AT:
      return big(staleness?.lastUserTouchAt);
    case F.STALENESS_LAST_REMOTE_CHANGE_AT:
      return big(staleness?.lastRemoteChangeAt);
    case F.STALENESS_STATUS_ENTERED_AT:
      return big(staleness?.statusEnteredAt);
    case F.STALENESS_WAITING_SINCE:
      return big(staleness?.waitingSince);
    default:
      // remote_extras has no value to compare.
      return undefined;
  }
}

/** A checked value operand as the evaluator compares it. */
function operandValue(v: FieldValue): Value {
  const k = v.kind;
  switch (k.case) {
    case "stringValue":
    case "markdownValue":
    case "isoWeek":
      return { k: "str", v: k.value };
    case "itemId":
      return { k: "str", v: k.value.toUpperCase() };
    case "numberValue":
      return { k: "num", v: k.value };
    case "uintValue":
      return { k: "big", v: k.value };
    case "timestampValue":
    case "durationValue":
      return { k: "big", v: nanosOf(k.value) };
    case "dateValue":
      return { k: "day", v: dayOf(k.value) };
    case "boolValue":
      return { k: "bool", v: k.value };
    case "priority":
      return { k: "prio", v: k.value };
    case "person":
      return person(k.value)!;
    default:
      return { k: "enum", v: k.value as number };
  }
}

/** Equality of two values of one target (FieldValue equality). */
function same(a: Value, b: Value): boolean {
  if (a.k === "person" && b.k === "person") return a.id === b.id && a.text === b.text;
  return a.k === b.k && "v" in a && "v" in b && a.v === b.v;
}

/** Code point order, as SQLite's BINARY collation compares UTF-8. */
export function compareCodePoints(a: string, b: string): number {
  const [x, y] = [a[Symbol.iterator](), b[Symbol.iterator]()];
  for (;;) {
    const [p, q] = [x.next(), y.next()];
    if (p.done || q.done) return p.done && q.done ? 0 : p.done ? -1 : 1;
    const d = p.value.codePointAt(0)! - q.value.codePointAt(0)!;
    if (d !== 0) return d < 0 ? -1 : 1;
  }
}

/** LOWEST 1 to HIGHEST 5; undefined for PRIORITY_NONE, which is outside the order. */
function rank(p: Priority): number | undefined {
  return p >= Priority.LOWEST && p <= Priority.HIGHEST ? p - 1 : undefined;
}

/** How two values of one target order (-1, 0, 1); undefined when they have no order, PRIORITY_NONE included. */
function order(a: Value, b: Value): number | undefined {
  const sign = (n: number | bigint) => (n < 0 ? -1 : n > 0 ? 1 : 0);
  if (a.k === "prio" && b.k === "prio") {
    const [x, y] = [rank(a.v), rank(b.v)];
    return x === undefined || y === undefined ? undefined : sign(x - y);
  }
  if (a.k === "str" && b.k === "str") return compareCodePoints(a.v, b.v);
  if (a.k === "big" && b.k === "big") return sign(a.v - b.v);
  if (a.k === "bool" && b.k === "bool") return sign(Number(a.v) - Number(b.v));
  if ((a.k === "num" || a.k === "day" || a.k === "enum") && a.k === b.k) return sign(a.v - (b as { v: number }).v);
  return undefined;
}

// ---- resolved filters and the evaluator -----------------------------------------

type RClause = { readonly pred: RPred } | { readonly allOf: readonly RClause[]; readonly anyOf: readonly RClause[] };

interface RPred {
  readonly field: ItemField;
  readonly op: Operator;
  readonly values: readonly Value[];
  /** MATCHES's text. */
  readonly text: string | undefined;
}

/** A checked filter resolved for one today, which it keeps for the defer rule and is_overdue. */
export interface Resolved {
  readonly allOf: readonly RClause[];
  readonly anyOf: readonly RClause[];
  readonly flags: Pick<Filter, "includeArchived" | "includeDone" | "includeDeferred" | "includeDeleted">;
  readonly today: number;
}

/**
 * The relative date on `today`: a day number, or the ISO week for
 * CURRENT_WEEK; undefined when that falls outside 0001-01-01 to 9999-12-31,
 * or for CURRENT_WEEK with an offset.
 */
function resolveRelative(anchor: Anchor, offsetDays: number, today: number): Value | undefined {
  const inRange = (n: number) => n >= dayNumber(1, 1, 1) && n <= dayNumber(9999, 12, 31);
  if (anchor === Anchor.CURRENT_WEEK) return offsetDays === 0 && inRange(today) ? { k: "str", v: isoWeek(today) } : undefined;
  const monday = today - (weekday(today) - 1);
  const base = anchor === Anchor.TODAY ? today : anchor === Anchor.START_OF_WEEK ? monday : monday + 6;
  const n = base + offsetDays;
  return inRange(n) ? { k: "day", v: n } : undefined;
}

/** `filter`, which checkFilter accepted, resolved for `today`; undefined when a relative date does not resolve on it. */
export function resolveFilter(filter: Filter, today: number): Resolved | undefined {
  try {
    return {
      allOf: filter.allOf.map((c) => resolveClause(c, today)),
      anyOf: filter.anyOf.map((c) => resolveClause(c, today)),
      flags: filter,
      today,
    };
  } catch (e) {
    if (e instanceof Invalid) return undefined;
    throw e;
  }
}

function resolveClause(c: Clause, today: number): RClause {
  switch (c.kind.case) {
    case "predicate": {
      const { target, op, operands } = c.kind.value;
      let text: string | undefined;
      const values: Value[] = [];
      for (const o of operands) {
        if (o.kind.case === "text") text = o.kind.value;
        else if (o.kind.case === "value") values.push(operandValue(o.kind.value));
        else if (o.kind.case === "relativeDate") {
          const v = resolveRelative(o.kind.value.anchor, o.kind.value.offsetDays, today);
          if (!v) throw new Invalid("", "out of range");
          values.push(v);
        }
      }
      return { pred: { field: target, op, values, text } };
    }
    case "group":
      return { allOf: c.kind.value.allOf.map((g) => resolveClause(g, today)), anyOf: c.kind.value.anyOf.map((g) => resolveClause(g, today)) };
    default:
      throw new Invalid("", "a clause with neither a predicate nor a group");
  }
}

/**
 * Whether `item` passes `filter` on its today: the implicit rules the
 * include_* flags do not lift (deleted, archived, done or cancelled,
 * deferred past today), then the clauses. An unset target never matches IN,
 * EQ or an ordering operator, and always matches NE.
 */
export function matches(item: Item, filter: Resolved): boolean {
  const { flags, today } = filter;
  const hidden =
    (!flags.includeDeleted && item.deletedAt !== undefined) ||
    (!flags.includeArchived && item.archived !== undefined) ||
    (!flags.includeDone && (item.status === Status.DONE || item.status === Status.CANCELLED)) ||
    (!flags.includeDeferred && item.deferUntil !== undefined && dayOf(item.deferUntil) > today);
  return !hidden && clausesMatch(item, filter.allOf, filter.anyOf, today);
}

function clausesMatch(item: Item, allOf: readonly RClause[], anyOf: readonly RClause[], today: number): boolean {
  const clause = (c: RClause) => ("pred" in c ? predicateMatches(item, c.pred, today) : clausesMatch(item, c.allOf, c.anyOf, today));
  return allOf.every(clause) && (anyOf.length === 0 || anyOf.some(clause));
}

function predicateMatches(item: Item, p: RPred, today: number): boolean {
  const value = valueOf(item, p.field, today);
  const equals = () => value !== undefined && p.values.some((x) => same(value, x));
  const members = value?.k === "set" ? value.v : [];
  const member = (x: Value) => x.k === "str" && members.includes(x.v);
  switch (p.op) {
    case Operator.IS_SET:
      return isSet(value);
    case Operator.IS_UNSET:
      return !isSet(value);
    case Operator.IN:
    case Operator.EQ:
      return equals();
    case Operator.NE:
      return !equals();
    case Operator.LT:
    case Operator.LE:
    case Operator.GT:
    case Operator.GE: {
      const x = p.values[0];
      const o = value === undefined || x === undefined ? undefined : order(value, x);
      if (o === undefined) return false;
      return p.op === Operator.LT ? o < 0 : p.op === Operator.LE ? o <= 0 : p.op === Operator.GT ? o > 0 : o >= 0;
    }
    case Operator.CONTAINS_ANY:
      return p.values.some(member);
    case Operator.CONTAINS_ALL:
      return p.values.length > 0 && p.values.every(member);
    case Operator.MATCHES:
      return p.text !== undefined && textMatches(searchText(item, p.field), p.text);
    default:
      return false;
  }
}

/** A set is set when it is non-empty; anything else when it has a value. */
function isSet(value: Value | undefined): boolean {
  if (value === undefined) return false;
  return value.k === "set" || value.k === "fields" ? value.v.length > 0 : true;
}

// ---- MATCHES ------------------------------------------------------------------

/** The full-text column MATCHES searches for `field`; labels as their JSON text, as the index has them. */
function searchText(item: Item, field: ItemField): string {
  switch (field) {
    case F.TITLE:
      return item.title;
    case F.DESCRIPTION:
      return item.description;
    case F.LABELS:
      return JSON.stringify(item.labels);
    default:
      return item.remoteLink?.key ?? "";
  }
}

/**
 * The words of `text`, approximating the index's tokenizer (unicode61 with
 * remove_diacritics 2): lower case, decomposed with the combining marks
 * stripped, and runs of letters and digits. Exact parity is not a goal.
 */
export function tokens(text: string): string[] {
  return text.toLowerCase().normalize("NFD").replace(/\p{M}/gu, "").match(/[\p{L}\p{N}]+/gu) ?? [];
}

/**
 * MATCHES `text` in one column (filter.proto): the text splits into terms on
 * code points up to U+0020; a term without words is dropped, and with none
 * left nothing matches. Otherwise every term's words must appear in the
 * column consecutively, the last as a prefix.
 */
function textMatches(column: string, text: string): boolean {
  const terms = text
    .split(/[\x00-\x20]+/)
    .map(tokens)
    .filter((t) => t.length > 0);
  if (terms.length === 0) return false;
  const words = tokens(column);
  const at = (term: string[], i: number) =>
    term.every((t, j) => {
      const w = words[i + j];
      return w !== undefined && (j === term.length - 1 ? w.startsWith(t) : w === t);
    });
  return terms.every((term) => words.some((_, i) => at(term, i)));
}

// ---- sorting and paging -----------------------------------------------------------

/** `sort`, then the tie-break created_at, id ascending without the keys `sort` already has. */
export function sortKeys(sort: readonly SortKey[]): SortKey[] {
  const keys = [...sort];
  for (const key of [F.CREATED_AT, F.ID]) {
    if (!sort.some((k) => k.key === key)) keys.push(create(SortKeySchema, { key }));
  }
  return keys;
}

/** A row's place in a sort: its value of each key, undefined when unset (PRIORITY_NONE too). */
export type SortTuple = readonly (Value | undefined)[];

export function sortTuple(item: Item, keys: readonly SortKey[], today: number): SortTuple {
  return keys.map((k) => {
    const v = valueOf(item, k.key, today);
    return v?.k === "prio" && rank(v.v) === undefined ? undefined : v;
  });
}

/** The order of two rows under `keys`: each key in its own direction, unset values last either way. */
export function compareTuples(a: SortTuple, b: SortTuple, keys: readonly SortKey[]): number {
  for (const [i, key] of keys.entries()) {
    const [x, y] = [a[i], b[i]];
    if (x === undefined || y === undefined) {
      if (x !== y) return x === undefined ? 1 : -1;
      continue;
    }
    const o = order(x, y) ?? 0;
    if (o !== 0) return key.descending ? -o : o;
  }
  return 0;
}

/**
 * The filter and sort a listing of `view` uses, narrowed by a request's
 * (item_service.proto ListItemsRequest.view_id): the view's filter AND the
 * request's clauses as one more group, each include flag set in either, and
 * the request's sort unless it is empty.
 */
export function narrow(view: View, filter: Filter | undefined, sort: readonly SortKey[]): { filter: Filter; sort: SortKey[] } {
  const merged = view.filter ? clone(FilterSchema, view.filter) : create(FilterSchema);
  if (filter) {
    // An empty group matches everything, so it is left out.
    if (filter.allOf.length > 0 || filter.anyOf.length > 0) {
      merged.allOf.push(create(ClauseSchema, { kind: { case: "group", value: { allOf: filter.allOf, anyOf: filter.anyOf } } }));
    }
    merged.includeArchived ||= filter.includeArchived;
    merged.includeDone ||= filter.includeDone;
    merged.includeDeferred ||= filter.includeDeferred;
    merged.includeDeleted ||= filter.includeDeleted;
  }
  return { filter: merged, sort: sort.length > 0 ? [...sort] : [...view.sort] };
}

/** FNV-1a (64-bit) of the effective filter and sort, which a page token is bound to. */
export function fingerprint(filter: Filter, keys: readonly SortKey[]): string {
  const bytes = [...toBinary(FilterSchema, filter), ...keys.flatMap((k) => [...toBinary(SortKeySchema, k), 0xff])];
  let h = 0xcbf29ce484222325n;
  for (const b of bytes) h = ((h ^ BigInt(b)) * 0x100000001b3n) & 0xffffffffffffffffn;
  return h.toString(16).padStart(16, "0");
}

/** What a page token holds: the listing it belongs to, the first page's today, and the last row's place. */
export interface PageCursor {
  readonly fingerprint: string;
  readonly today: number;
  readonly after: SortTuple;
}

const TOKEN_PREFIX = "fake1.";

export function encodeCursor(c: PageCursor): string {
  const json = JSON.stringify({ f: c.fingerprint, t: c.today, a: c.after.map((v) => v ?? null) }, (_, v: unknown) =>
    typeof v === "bigint" ? { big: v.toString() } : v,
  );
  return TOKEN_PREFIX + Buffer.from(json).toString("base64url");
}

/** A token encodeCursor made, or undefined for anything else. */
export function decodeCursor(token: string): PageCursor | undefined {
  if (!token.startsWith(TOKEN_PREFIX)) return undefined;
  try {
    const json = Buffer.from(token.slice(TOKEN_PREFIX.length), "base64url").toString();
    const raw = JSON.parse(json, (_, v: unknown) => (v !== null && typeof v === "object" && "big" in v ? BigInt((v as { big: string }).big) : v)) as {
      f: unknown;
      t: unknown;
      a: unknown;
    };
    if (typeof raw.f !== "string" || !Number.isInteger(raw.t) || !Array.isArray(raw.a)) return undefined;
    const after = raw.a.map((v: unknown) => (v === null ? undefined : (v as Value)));
    if (!after.every((v) => v === undefined || (typeof v === "object" && typeof v.k === "string"))) return undefined;
    return { fingerprint: raw.f, today: raw.t as number, after };
  } catch {
    return undefined;
  }
}
