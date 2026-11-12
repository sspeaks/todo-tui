import { type MessageInitShape, clone, create, equals } from "@bufbuild/protobuf";
import { type Timestamp, TimestampSchema } from "@bufbuild/protobuf/wkt";
import { Code, ConnectError, type Transport, createRouterTransport } from "@connectrpc/connect";
import { BadRequestSchema, ErrorInfoSchema } from "./gen/google/rpc/error_details_pb.js";
import { DateSchema } from "./gen/google/type/date_pb.js";
import { type MutationResultSchema, type PersonRef, PersonRefSchema, Topic } from "./gen/todo/v1/common_pb.js";
import { BackupConfigSchema, type ConfigSnapshot, ConfigSnapshotSchema, UiConfigSchema } from "./gen/todo/v1/config_pb.js";
import { ItemField } from "./gen/todo/v1/field_pb.js";
import {
  type Clause as ClauseMessage,
  type Filter,
  FilterSchema,
  type OperandSchema,
  Operator,
  RelativeDate_Anchor,
  type SortKey,
  SortKeySchema,
} from "./gen/todo/v1/filter_pb.js";
import {
  ArchiveReason,
  ArchivedSchema,
  CreatedVia,
  type Item,
  ItemRecurrenceSchema,
  Energy,
  ItemSchema,
  LinkState,
  Priority,
  RecurrenceRuleSchema,
  ResolutionKind,
  Status,
  StatusCategory,
  TodoRefinement,
  type Waiting,
  WaitingSchema,
  Weekday,
} from "./gen/todo/v1/item_pb.js";
import {
  type AddRelationRequest,
  type CaptureDraft,
  type CaptureItemRequest,
  type GetStatusCountsRequest,
  GetActivityReportResponseSchema,
  type GetStatusCountsResponseSchema,
  ItemService,
  type ListItemsRequest,
  type ListItemsResponseSchema,
  type Mutation,
  type ParseCaptureRequest,
  type ParseDateRequest,
  type ParseDateResponseSchema,
  type ParseRecurrenceRequest,
  type ParseRecurrenceResponse,
  type RedoRequest,
  type RemoveRelationRequest,
  type SetRecurrenceRequest,
  type UndoRequest,
  type UpdateItemRequest,
} from "./gen/todo/v1/item_service_pb.js";
import { OpKind, type OpSummary, OpSummarySchema, type UndoSkip, UndoSkipReason, UndoSkipSchema } from "./gen/todo/v1/op_pb.js";
import { type Relation, RelationKind, RelationSchema } from "./gen/todo/v1/relation_pb.js";
import { SystemService } from "./gen/todo/v1/system_service_pb.js";
import { type View, ViewSchema } from "./gen/todo/v1/view_pb.js";
import {
  type CreateViewRequest,
  type CreateViewResponseSchema,
  type DeleteViewRequest,
  type DeleteViewResponseSchema,
  type MoveViewRequest,
  type MoveViewResponseSchema,
  type ParseQueryRequest,
  type ParseQueryResponseSchema,
  type UpdateViewRequest,
  type UpdateViewResponseSchema,
  ViewService,
} from "./gen/todo/v1/view_service_pb.js";
import { type Change, ChangeSchema, type SubscribeResponse, SubscribeResponseSchema, WatchService } from "./gen/todo/v1/watch_service_pb.js";
import { type FieldValue } from "./gen/todo/v1/field_value_pb.js";
import { type Patch, type PatchSchema, PatchState, type Suggestion, SuggestionKind, SuggestionSchema, SuggestionSource, SuggestionState } from "./gen/todo/v1/suggestion_pb.js";
import { type AssistantJob, AssistantJobSchema, AssistantJobState, AssistantService } from "./gen/todo/v1/assistant_service_pb.js";
import { SuggestionService } from "./gen/todo/v1/suggestion_service_pb.js";
import { type OutboxEntry, OutboxEntrySchema, type SyncStatus, SyncStatusSchema, SyncService } from "./gen/todo/v1/sync_service_pb.js";
import { parseCaptureText } from "./capture.js";
import { meeting, teamsUrl } from "./meeting.js";
import { civil, dayIn, dayOf, epochMs, isDate, parseDateExpr, protoDate, rustTrim, weekday } from "./dates.js";
import { DEFAULT_QUERIES, defaultViews } from "./default-views.js";
import {
  type FilterProblem,
  checkFilter,
  checkSort,
  compareTuples,
  decodeCursor,
  effectivePriority,
  encodeCursor,
  fingerprint,
  isOverdue,
  matches as passes,
  narrow,
  resolveFilter,
  sortKeys,
  sortTuple,
} from "./filter-eval.js";
import { DEFAULT_PAGE_SIZE, MAX_CAPTURE_BYTES, MAX_PAGE_SIZE } from "./limits.js";
import { type Rule, firstOnOrAfter, isFixed, nextInstance, parseRecurrenceText, ruleFromProto, ruleToProto } from "./recurrence.js";
import { ulid } from "./ulid.js";

type Waiter = (msg: SubscribeResponse | Error) => void;

type MutationResult = MessageInitShape<typeof MutationResultSchema>;

/** An Item's fields by localName. */
type Fields = Record<string, unknown>;

interface MutationResponse {
  item: Item;
  result: MutationResult;
  /** The next instances the op created (design 3.8). */
  generated: Item[];
}

/** Who made an op: the user for every RPC, the system for put and update. */
export type FakeActor = "user" | "system";

/**
 * One change an op made, as the daemon records events. `field` is the Item
 * field's localName (e.g. "title", "statusCategory"), and `old` / `new` are
 * the field's whole values (undefined when unset). A relation event's
 * `itemId` is the relation's from end.
 */
export type FakeEvent =
  | { readonly kind: "create"; readonly itemId: string }
  | { readonly kind: "set"; readonly itemId: string; readonly field: string; readonly old: unknown; readonly new: unknown }
  | { readonly kind: "relationAdd" | "relationRemove"; readonly itemId: string; readonly relation: Relation };

/**
 * Where an op stands on the undo stack (design 3.10): "done" on the undo
 * stack, "undone" on the redo stack; it leaves both as "dropped" (an undo or
 * redo restored nothing) or "discarded" (a new entry cleared the redo stack).
 */
export type FakeUndoState = "done" | "undone" | "dropped" | "discarded";

/** One op of FakeDaemon's log. */
export interface FakeOp {
  /** MutationResult.op_id. */
  readonly id: string;
  /** 1, 2, ... in log order. */
  readonly seq: number;
  readonly kind: OpKind;
  readonly actor: FakeActor;
  /** When it was recorded. */
  readonly at: Timestamp;
  /** OpSummary.description, as the daemon's summary.rs words it. */
  readonly summary: string;
  readonly events: readonly FakeEvent[];
  /** The op's last revision (the current one when it allocated none). */
  readonly revision: bigint;
  /** Unset for an op that never was an undo stack entry. */
  undoState: FakeUndoState | undefined;
  /** For an Undo or Redo: the entry it acted on. */
  readonly entryOp?: string;
  /** For an Undo or Redo: what it left alone, returned again on a replay. */
  readonly skipped?: readonly UndoSkip[];
}

/** A request remembered by its request_id, for replays. */
interface Recorded {
  /** The RPC it was. */
  rpc: string;
  /**
   * What it targeted: the Item (a batch's first, a capture's new one), the
   * relation as stored (relationKey), "" for Undo and Redo.
   */
  id: string;
  result: MutationResult;
  op: FakeOp;
}

type MutationKind = "update" | "setStatus" | "setRecurrence" | "archive" | "unarchive" | "delete" | "undelete";

/** One single-item mutation, checked and converted as the daemon's request layer does before the store sees it. */
interface Planned {
  kind: MutationKind;
  id: string;
  expectedRevision: bigint | undefined;
  /** What its errors' request paths start with: "" for a single-item RPC, e.g. "mutations[2].update_item." in a batch. */
  prefix: string;
  /** The request path of its Item id, e.g. "id" or "mutations[2].update_item.item.id". */
  idPath: string;
  /** Whether it is part of a BatchMutate, whose precondition failures name `idPath`. */
  batched: boolean;
  /** The person confirmed a pushed edit of someone else's issue (design 5.9). */
  confirmNotMine?: boolean;
  /** UpdateItem: the mask's paths, in order, and the request's values for them, by path. */
  mask?: { paths: readonly string[]; values: Fields };
  /** SetStatus: the target. */
  status?: Status;
  /**
   * SetRecurrence: the rule (undefined clears it), the series a rule starts
   * on an Item that does not recur yet, and the day a fixed rule's first
   * occurrence is counted from when the Item has no due date.
   */
  recurrence?: { rule: Rule | undefined; seriesId: string; today: number };
}

/** What an op did to one Item's status_entered_at: FakeDaemon's private record of when each Item closed. */
interface Entered {
  id: string;
  /** In nanoseconds; undefined when it was unknown. */
  old: bigint | undefined;
  new: bigint | undefined;
}

/** One field change of a mutation or restore. */
interface FieldChange {
  field: string;
  old: unknown;
  new: unknown;
}

/** Writes `value` into `field` if the field still holds `expected` (string sets compare as sets). */
interface FieldRestore {
  field: string;
  expected: unknown;
  value: unknown;
}

/** One step of an undo or redo: a unit of restores applied all or nothing, or a relation step. */
type InverseStep =
  | { kind: "restore"; itemId: string; unit: FieldRestore[] }
  | { kind: "addRelation" | "removeRelation"; relation: Relation };

const NANOS = 1_000_000_000n;

/** The most mutations one BatchMutate may carry. */
const MAX_BATCH = 1000;

const DAY_MS = 24 * 60 * 60 * 1000;

/** How long a done or cancelled Item stays out of Archive by default: 7 days (design 3.7), in milliseconds. */
export const ARCHIVE_DONE_AFTER_MS = 7 * DAY_MS;

/** The daemon's config with no config file: every value a default. */
export function defaultConfig(path = "/fake/config.toml"): ConfigSnapshot {
  return create(ConfigSnapshotSchema, {
    path,
    exists: false,
    configVersion: 1,
    ui: DEFAULT_UI,
    backup: DEFAULT_BACKUP,
  });
}

const DEFAULT_UI = { reviewDay: Weekday.MONDAY, archiveDoneAfterDays: 7 };
const DEFAULT_BACKUP = { enabled: true, interval: "daily", retainDays: 14 };

/** A message init's own fields, without the message marker. */
function strip<T extends object>(init: T): Omit<T, "$typeName" | "$unknown"> {
  const { $typeName: _t, $unknown: _u, ...rest } = init as T & { $typeName?: unknown; $unknown?: unknown };
  return rest;
}

/** The most Items one auto-archive op changes (archive.rs AUTO_ARCHIVE_LIMIT). */
const AUTO_ARCHIVE_LIMIT = 1000;

/** What one {@link FakeDaemon.runScheduler} sweep changed. */
export interface FakeSweep {
  /** The Items archived AUTO_DONE, oldest close first. */
  archived: string[];
  /** The Items archived AUTO_DONE that were open again and are unarchived, in archive order. */
  healed: string[];
  /** The ops it recorded, each of at most 1000 Items; none when nothing was due. */
  ops: FakeOp[];
}

/** The stored fields behind the derived status, in SetStatus event order (design 3.4). */
const TRIPLE: readonly string[] = ["statusCategory", "resolutionKind", "todoRefinement"];

type Triple = Pick<Item, "statusCategory" | "resolutionKind" | "todoRefinement">;

const INBOX_TRIPLE: Triple = { statusCategory: StatusCategory.TODO, resolutionKind: undefined, todoRefinement: TodoRefinement.INBOX };

const FIELDS = new Map(ItemSchema.fields.map((f) => [f.localName, f]));

/** Item fields that put's system op does not record events for. */
const UNTRACKED = new Set(["revision", "updatedAt", "status", "relations", "subtasksTotal", "subtasksDone", "parentTitle"]);

/** What an Item adds to its parent's counts (hierarchy.rs `contribution`): a change re-emits the old and new parent. */
function contribution(item: Item | undefined): string {
  if (!item) return "";
  const counted = item.deletedAt === undefined && item.status !== Status.CANCELLED;
  return `${item.parentId ?? ""}:${counted}:${counted && item.status === Status.DONE}`;
}

/** The closed statuses: an Item in one is in the done category. */
const CLOSED = new Set([Status.DONE, Status.CANCELLED]);

/** Done or cancelled, as Status::is_closed has it. */
function isClosed(item: Item): boolean {
  return item.statusCategory === StatusCategory.DONE;
}

/** A timestamp in nanoseconds; 0 when unset. */
function nanosOf(t: Timestamp | undefined): bigint {
  return t === undefined ? 0n : t.seconds * NANOS + BigInt(t.nanos);
}

function compareBig(a: bigint, b: bigint): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

/** Summaries cut titles to this many characters, ellipsis included (summary.rs). */
const MAX_TITLE_CHARS = 40;

const STATUS_VERBS: Record<number, string> = {
  [Status.INBOX]: "move to inbox",
  [Status.NEXT]: "move to next",
  [Status.SOMEDAY]: "move to someday",
  [Status.IN_PROGRESS]: "start",
  [Status.DONE]: "mark done",
  [Status.CANCELLED]: "cancel",
};

const RELATION_WORDS: Record<number, string> = {
  [RelationKind.BLOCKS]: "blocks",
  [RelationKind.DUPLICATES]: "duplicates",
  [RelationKind.RELATES_TO]: "relates to",
};

/** FakeDaemon's clock and zone. */
export interface FakeDaemonOptions {
  /**
   * "Now": today when a request gives no reference_time, and the time of
   * every write (default: the system clock). Writes get strictly increasing
   * times, as a real clock gives them, so updated_at orders them.
   */
  now?: () => Date;
  /** The IANA zone today is taken in, as the daemon takes its system zone (default: this process's). */
  timeZone?: string;
}

/**
 * An in-memory daemon over createRouterTransport for tests: ItemService,
 * ViewService, WatchService with replay / CaughtUp / ResyncRequired and
 * topics, and SystemService.GetServerInfo. It follows the daemon's ItemService
 * conventions and the store's rules (crates/todo-store), checked by the
 * shared conformance suite in test/conformance:
 *
 * - ULID request ids and Item ids; request_id replay (the original result
 *   with the current state) and reuse checks; expected_revision; soft-deleted
 *   Items take only DeleteItem (a no-op) and UndeleteItem; a mutation that
 *   changes nothing allocates no revision.
 * - Items carry status, effective_priority and is_overdue as derived when
 *   read, for the request's today (in {@link FakeDaemonOptions.timeZone}),
 *   and the subtask counts and parent title, with the store's fan-out: a
 *   change to them re-emits the Item without touching its updated_at.
 * - ListItems with the whole filter language (filter-eval.ts, ported from
 *   todo_core::filter): the implicit rules, the checks with the daemon's
 *   error paths and its INVALID / UNIMPLEMENTED split, relative dates,
 *   sorts with the created_at, id tie-break, keyset page tokens bound to the
 *   effective filter and sort that keep the first page's today, and view_id
 *   (VIEW_NOT_FOUND). GetStatusCounts, and ParseDate with the capture date
 *   grammar.
 * - The six default views (todo_core::view::defaults()), seeded first as
 *   the store seeds them, each with a revision and a TOPIC_VIEW Change;
 *   ListViews. CreateView, UpdateView, DeleteView and MoveView as the
 *   store's view.rs: ops for their request_id that are never undo stack
 *   entries, names unique ignoring case, positions dense, every view that
 *   moves with a revision and a Change. A view's query text is the
 *   daemon's printer's, which the fake does not have: it knows the default
 *   views' and what defineQuery gives, and is empty otherwise.
 * - GetActivityReport answers what setActivityReport gave (an empty
 *   report until then): the report's rules are the daemon's alone.
 * - An op log ({@link ops}): each touched Item gets one revision and one
 *   Change, and each changed field one event. BatchMutate applies its
 *   mutations in order as one op, all or nothing, under the single-item
 *   RPCs' rules.
 * - The global undo stack: an op by the user that changed something becomes
 *   the top entry and discards the redo stack; Undo and Redo (with the
 *   expected_op_id guard) restore field values only where the field still
 *   holds the value the reverted op wrote, the status triple as one unit
 *   and labels as a set, and report the rest as UndoSkips (CHANGED_SINCE,
 *   ITEM_DELETED, INVALID); the parent rule holds where the whole undo
 *   leaves the Items, and a step that breaks it there is skipped; one that
 *   restores nothing drops its entry; an undone capture is soft-deleted;
 *   replays return the original entry and skips. GetUndoState, and
 *   OpSummary texts and error messages as the daemon words them.
 * - Parent rules (design 3.7): DeleteItem soft-deletes the live
 *   descendants in the same op; UndeleteItem restores the ones deleted by
 *   the Item's delete, and fails with PARENT_DELETED while the parent is
 *   deleted (in a batch, as the whole batch leaves it); CaptureItem and
 *   UpdateItem check parent_id (missing, deleted, the Item itself, a cycle).
 * - Relations (design 3.6): canonical RELATES_TO, no-op adds and removes,
 *   both ends bumped and published, Item.relations on both ends; a
 *   relation outlives a deleted end, and undo and redo change it anyway.
 * - Capture (design 2.3): ParseCapture, and CaptureItem parsing its text,
 *   with todo_core::capture ported (capture.ts) on the reference day: the
 *   tokens with code-point offsets, the warnings, last-wins, the escape;
 *   a ~ token waits on free text since that day. created_via must be TUI,
 *   CLI or MCP; a replay by request_id or spool_id returns the original
 *   capture, with the text parsed on the day the Item was created.
 * - Recurrence (design 3.8), with todo_core::recurrence ported
 *   (recurrence.ts): an @ token starts a new series, due on its first date
 *   unless a ^ token set one. SetRecurrence (alone and in a batch) sets,
 *   replaces (same series) or clears the rule, with the daemon's rule
 *   paths and ITEM_CLOSED; ParseRecurrence. Completing an instance (open
 *   before the op, done or cancelled after it, live, still with a rule)
 *   creates the next one in the same op, as generate_tx does: a post-pass on
 *   the op's final state, guarded by any other live, open instance of the
 *   series. It is returned as `generated` (also on a replay), named in the
 *   op's events and summary, re-emits its parent, and is soft-deleted by an
 *   undo and undeleted by a redo.
 * - The scheduler's auto_done archive and its healing, run on demand by
 *   {@link runScheduler}, after the config's archive_done_after_days by
 *   default; SetStatus to an open status clears an auto_done archive itself.
 * - The config (design 8.2): GetConfig serves {@link config}; ReloadConfig
 *   answers unchanged (there is no file); {@link setConfig} stands for an
 *   edited file being reloaded and publishes a TOPIC_CONFIG Change. As the
 *   daemon's, a config change carries the config current when it is sent,
 *   also on replay.
 * - UpdateItem checks its mask's paths as the daemon does: Item field paths
 *   only, each field once; status, archived, relations and read-only paths
 *   are refused. Of the values it checks titles (stored trimmed, then
 *   non-empty, at most 64 KiB and one line), labels (non-empty, with no
 *   whitespace or control character, deduplicated), parent_id and a
 *   waiting's `on` (free text stored trimmed, not empty);
 *   a waiting without `since` is waiting since today. A nested waiting
 *   path changes only that field and fails while waiting is unset.
 *   SetStatus checks its status and decomposes it into the triple as the
 *   daemon does.
 *
 * put and update are the tests' off-stack changes: an op by the system,
 * never an undo stack entry and leaving the redo stack alone, so a later
 * Undo skips what they changed. remove is a hard removal.
 *
 * What it does not model: other field values are not checked (enums other
 * than priority and energy, dates,
 * numbers, estimates, focus_week, people, and the tracker-only fields of an
 * unlinked Item are taken as given). An op records a changed top-level
 * field as one event, so a nested waiting path records, and its undo
 * restores, the whole `waiting`. Staleness signals are not maintained or
 * restored (`updated_at` is set on every op that touches an Item), except
 * that the fake records privately when each Item entered its status, for
 * the scheduler's clock, and undo and redo restore that with the status;
 * put enters a new status at the Item's updated_at, so a test can
 * back-date a close. There is no background scheduler: archives happen
 * only when a test calls runScheduler. Request ids never expire. MATCHES
 * approximates the full-text index (lower case, accents stripped, runs of
 * letters and digits), so it agrees on ASCII and Latin text but not on
 * every script or symbol.
 */
export class FakeDaemon {
  revision = 0n;
  readonly items = new Map<string, Item>();
  /** The saved views, by id: the six defaults, seeded by the constructor. */
  readonly views = new Map<string, View>();
  readonly changes: Change[] = [];
  /** Every op, oldest first: each RPC call that got past validation (no-ops too), each put and update. */
  readonly ops: FakeOp[] = [];
  /** Subscribing with since_revision below this yields ResyncRequired. */
  replayFloor = 0n;
  /** When true every RPC fails with UNAVAILABLE. */
  down = false;
  /** The largest page ListItems returns (the daemon's is MAX_PAGE_SIZE); page_size 0 asks for DEFAULT_PAGE_SIZE. */
  maxPageSize = MAX_PAGE_SIZE;
  /** Runs before every ListItems; throw from it to fail that call. */
  onListItems: ((req: ListItemsRequest) => void) | undefined;
  apiMinor = 27;
  /** What Export streams, as one batch; unset, Export is UNIMPLEMENTED (the format is the daemon's). */
  exportLines: string[] | undefined;
  /** What GetConfig serves; change it with {@link setConfig}. */
  config: ConfigSnapshot = defaultConfig();
  readonly calls = { listItems: 0, listViews: 0, getStatusCounts: 0, subscribe: [] as (bigint | undefined)[] };
  readonly transport: Transport;
  readonly now: () => Date;
  readonly timeZone: string;
  private readonly watchers = new Set<Waiter>();
  /** Requests by request_id (upper case), for replays. */
  private readonly requests = new Map<string, Recorded>();
  /** Captures by spool_id (upper case), for replays. */
  private readonly spools = new Map<string, Recorded>();
  /** What GetActivityReport answers: setActivityReport's. */
  /** What GetSyncStatus answers: setSyncStatus's, by instance. */
  private syncStatus = new Map<string, SyncStatus>();
  /** SyncNow calls, by instance ("" for all). */
  readonly syncNows: string[] = [];
  /** The Suggestions, by id: putSuggestion's, as the RPCs leave them. */
  readonly suggestions = new Map<string, Suggestion>();
  /** The issues attach can link to (setIssue), and the last made number. */
  private readonly issues = new Map<string, { title: string }>();
  private issueSeq = 100;
  /** The assistant's jobs (Refine), oldest first. */
  private readonly jobs = new Map<string, AssistantJob>();
  private refineAnswer: (item: Item) => { rationale: string; patches: MessageInitShape<typeof PatchSchema>[] } | { error: string } = () => ({ rationale: "", patches: [] });
  /** What ListOutbox answers: setOutbox's. */
  private outbox: OutboxEntry[] = [];
  /** The patches each apply op applied, by op id: its undo reopens them, a redo applies them again. */
  private readonly appliedBy = new Map<string, Patch[]>();
  private activityReport: MessageInitShape<typeof GetActivityReportResponseSchema> = { text: "L48:\n- (nothing)\nN48:\n- (nothing)\n" };
  /** What ParseQuery reads, by text: the default views' and defineQuery's. */
  private readonly queries = new Map<string, { filter: Filter; sort: SortKey[]; showCount: boolean }>();
  /** Every relation, by relationKey. */
  private readonly relations = new Map<string, Relation>();
  private nextId = 0;
  /** The last write time handed out, in nanoseconds. */
  private lastWrite = -(2n ** 63n);
  /**
   * Each Item's status_entered_at, in nanoseconds: when an op last changed
   * its derived status, so for a closed Item when it closed (done →
   * cancelled closes it again). The fake keeps no staleness signals on its
   * Items, so it records them here, for the scheduler's clock. An Item
   * without an entry falls back to its updated_at, as a row from before the
   * signal was recorded does.
   */
  private readonly statusEnteredAt = new Map<string, bigint>();
  /** By op id, what each op did to statusEnteredAt, for undo and redo to restore it. */
  private readonly statusEnteredBy = new Map<string, Entered[]>();

  constructor(initial: MessageInitShape<typeof ItemSchema>[] = [], options: FakeDaemonOptions = {}) {
    this.now = options.now ?? (() => new Date());
    this.timeZone = options.timeZone ?? Intl.DateTimeFormat().resolvedOptions().timeZone;
    this.seedViews();
    for (const [i, init] of defaultViews().entries()) this.defineQuery(DEFAULT_QUERIES[i] ?? "", init);
    for (const init of initial) this.put(init);
    // A parent seeded after its subtasks counts them too.
    for (const item of this.items.values()) this.hierarchy(item);
    this.transport = createRouterTransport(({ service }) => {
      service(SystemService, {
        getServerInfo: () => {
          this.check();
          return { daemonVersion: "fake", apiMinor: this.apiMinor, capabilities: [] };
        },
        shutdown: () => ({}),
        getConfig: () => {
          this.check();
          return { config: this.config, revision: this.revision };
        },
        reloadConfig: () => {
          this.check();
          return { config: this.config, changed: false };
        },
        // There is no file to copy: the daemon's backups are its own.
        backup: () => {
          this.check();
          throw new ConnectError("FakeDaemon takes no backups", Code.Unimplemented);
        },
        // Lines a test gives in exportLines (the format is the daemon's).
        export: () => {
          this.check();
          const lines = this.exportLines;
          if (lines === undefined) throw new ConnectError("FakeDaemon does not export", Code.Unimplemented);
          return (async function* () {
            yield { lines };
          })();
        },
        import: () => {
          this.check();
          throw new ConnectError("FakeDaemon does not import", Code.Unimplemented);
        },
      });
      service(ItemService, {
        captureItem: (req) => this.capture(req),
        parseCapture: (req) => {
          this.check();
          return this.parseCapture(req);
        },
        parseDate: (req) => {
          this.check();
          return this.parseDate(req);
        },
        listItems: (req) => {
          this.check();
          this.calls.listItems++;
          this.onListItems?.(req);
          return this.list(req);
        },
        getStatusCounts: (req) => {
          this.check();
          this.calls.getStatusCounts++;
          return this.statusCounts(req);
        },
        getItem: (req) => {
          this.check();
          return { item: this.present(this.get(idOf("id", req.id)), this.today()), revision: this.revision };
        },
        updateItem: (req) =>
          this.single("UpdateItem", OpKind.UPDATE, req.requestId, () => ({ ...planUpdate(req, "", this.today()), confirmNotMine: req.confirmNotMine })),
        setStatus: (req) => {
          // Closing an instance creates at most one next instance (design 3.8).
          const { generated, ...rest } = this.single("SetStatus", OpKind.SET_STATUS, req.requestId, () => ({
            ...planSetStatus(req, ""),
            confirmNotMine: req.confirmNotMine,
          }));
          return { ...rest, generated: generated[0] };
        },
        setRecurrence: (req) =>
          this.single("SetRecurrence", OpKind.SET_RECURRENCE, req.requestId, () => ({ ...planSetRecurrence(req, "", this.today()), confirmNotMine: req.confirmNotMine })),
        parseRecurrence: (req) => {
          this.check();
          return this.parseRecurrence(req);
        },
        buildMeetingLink: (req) => {
          this.check();
          if (this.config.meetings?.provider === "none") {
            throw errorInfo(Code.FailedPrecondition, "MEETINGS_OFF", 'meetings are off: [meetings] provider = "none"', {});
          }
          if (req.ids.length < 1 || req.ids.length > MAX_BATCH) throw badRequest("ids", `must hold 1 to ${MAX_BATCH} ids`);
          const items = req.ids.map((id, i) => {
            const item = this.items.get(idOf(`ids[${i}]`, id));
            if (!item || item.deletedAt !== undefined) throw notFound(idOf(`ids[${i}]`, id));
            return item;
          });
          const m = meeting(items, req.subject);
          return { url: teamsUrl(m), ...m };
        },
        getActivityReport: () => {
          this.check();
          return create(GetActivityReportResponseSchema, { ...this.activityReport, today: create(DateSchema, protoDate(this.today())), revision: this.revision });
        },
        archiveItem: (req) => this.single("ArchiveItem", OpKind.ARCHIVE, req.requestId, () => planItemOp("archive", req, "")),
        unarchiveItem: (req) => this.single("UnarchiveItem", OpKind.UNARCHIVE, req.requestId, () => planItemOp("unarchive", req, "")),
        deleteItem: (req) => this.single("DeleteItem", OpKind.DELETE, req.requestId, () => planItemOp("delete", req, "")),
        undeleteItem: (req) => this.single("UndeleteItem", OpKind.UNDELETE, req.requestId, () => planItemOp("undelete", req, "")),
        batchMutate: (req) => {
          this.check();
          const requestId = requestIdOf(req.requestId);
          if (req.mutations.length < 1 || req.mutations.length > MAX_BATCH) {
            throw badRequest("mutations", `must hold 1 to ${MAX_BATCH} mutations`);
          }
          const today = this.today();
          const planned = req.mutations.map((m, i) => ({ ...planMutation(m, `mutations[${i}].`, today), confirmNotMine: req.confirmNotMine }));
          return this.mutate("BatchMutate", OpKind.BATCH, requestId, planned);
        },
        undo: (req) => this.undoTx("Undo", req, false),
        redo: (req) => this.undoTx("Redo", req, true),
        getUndoState: () => {
          this.check();
          return { undo: this.summaryOf(this.top(false)), redo: this.summaryOf(this.top(true)) };
        },
        addRelation: (req) => this.relationTx("AddRelation", req, true),
        removeRelation: (req) => this.relationTx("RemoveRelation", req, false),
      });
      service(SyncService, {
        getSyncStatus: () => {
          this.check();
          return { instances: [...this.syncStatus.values()] };
        },
        syncNow: (req) => {
          this.check();
          // As the daemon decides it: by the config's instances.
          const ids = this.config.instances.map((i) => i.id);
          if (ids.length === 0 || (req.instanceId && !ids.includes(req.instanceId))) {
            throw errorInfo(Code.FailedPrecondition, "NO_INSTANCE", "no [[instances]] are configured to sync", {});
          }
          this.syncNows.push(req.instanceId);
          return { request: BigInt(this.syncNows.length) };
        },
        listOutbox: (req) => {
          this.check();
          return { entries: this.outbox.filter((e) => !req.instanceId || e.instanceId === req.instanceId) };
        },
        promote: (req) => this.link(req.itemId, undefined, req.project),
        attach: (req) => this.link(req.itemId, req.key.trim().toUpperCase(), undefined),
      });
      service(SuggestionService, {
        listSuggestions: (req) => {
          this.check();
          const item = req.itemId ? idOf("item_id", req.itemId) : undefined;
          const waiting = [...this.suggestions.values()].filter(
            (s) => waits(s) && (item === undefined || s.itemId === item) && this.items.get(s.itemId)?.deletedAt === undefined,
          );
          return { suggestions: waiting, revision: this.revision };
        },
        applySuggestionPatches: (req) => this.applyPatches(req.requestId, req.patchIds, req.confirmNotMine),
        rejectSuggestion: (req) => this.rejectSuggestion(req.requestId, req.id),
      });
      service(AssistantService, {
        refine: (req) => this.refine(req.itemIds),
        getJob: (req) => {
          this.check();
          const job = this.jobs.get(idOf("id", req.id));
          if (!job) throw errorInfo(Code.NotFound, "JOB_NOT_FOUND", "no such job", {});
          return { job };
        },
        listJobs: () => {
          this.check();
          return { jobs: [...this.jobs.values()].reverse() };
        },
      });
      service(ViewService, {
        listViews: () => {
          this.check();
          this.calls.listViews++;
          return { views: this.tabOrder(), revision: this.revision };
        },
        parseQuery: (req) => this.parseQuery(req),
        createView: (req) => this.createView(req),
        updateView: (req) => this.editView(req),
        deleteView: (req) => this.deleteView(req),
        moveView: (req) => this.moveView(req),
      });
      const self = this;
      service(WatchService, {
        async *subscribe(req, ctx) {
          self.check();
          self.calls.subscribe.push(req.sinceRevision);
          const wants = topicFilter(req.topics);
          const queue: (SubscribeResponse | Error)[] = [];
          let wake = () => {};
          const waiter: Waiter = (m) => {
            if (!(m instanceof Error) && m.message.case === "change" && !wants(m.message.value)) return;
            queue.push(m);
            wake();
          };
          self.watchers.add(waiter);
          const onAbort = () => wake();
          ctx.signal.addEventListener("abort", onAbort);
          try {
            const since = req.sinceRevision;
            if (since !== undefined) {
              if (since < self.replayFloor || since > self.revision) {
                yield msg({ case: "resyncRequired", value: { currentRevision: self.revision } });
              } else {
                for (const c of self.changes) if (c.revision > since && wants(c)) yield msg({ case: "change", value: self.asSent(c) });
              }
            }
            yield msg({ case: "caughtUp", value: { revision: self.revision } });
            // The sync state as it is, as the daemon sends it after CaughtUp
            // (a later setSyncStatus reaches it through the watchers).
            for (const s of self.syncStatus.values()) yield msg({ case: "notice", value: { kind: { case: "syncStatus", value: s } } });
            // A status queued before the snapshot is in it: not sent twice.
            for (let i = queue.length - 1; i >= 0; i--) {
              const q = queue[i];
              if (!(q instanceof Error) && q?.message.case === "notice" && q.message.value.kind.case === "syncStatus") queue.splice(i, 1);
            }
            while (!ctx.signal.aborted) {
              const next = queue.shift();
              if (next instanceof Error) throw next;
              if (next) yield next;
              else await new Promise<void>((r) => (wake = r));
            }
          } finally {
            ctx.signal.removeEventListener("abort", onAbort);
            self.watchers.delete(waiter);
          }
        },
      });
    });
  }

  get openStreams(): number {
    return this.watchers.size;
  }

  /**
   * Creates or replaces an item and publishes a Change, as an op by the
   * system: an off-stack change that is never an undo stack entry and leaves
   * the redo stack alone. A status given without the status triple is
   * decomposed as SetStatus does. Relations come from the relation RPCs:
   * any in `init` are ignored.
   */
  put(init: MessageInitShape<typeof ItemSchema>): Item {
    this.revision++;
    const id = init.id || this.newId();
    const prev = this.items.get(id);
    const now = this.stamp();
    const item = prev ? merged(prev, init) : create(ItemSchema, { status: Status.INBOX, createdAt: now, ...init });
    item.id = id;
    settleStatus(item, Object.keys(init), prev);
    if (init.updatedAt === undefined) item.updatedAt = now;
    item.relations = this.relationsOf(id);
    item.revision = this.revision;
    this.settle(item);
    // A new status is entered at the Item's updated_at, which a test may set
    // to back-date it for the scheduler.
    if (prev?.status !== item.status) this.statusEnteredAt.set(id, nanosOf(item.updatedAt));
    this.items.set(id, item);
    this.hierarchy(item);
    this.publish({ revision: item.revision, topic: Topic.ITEM, id, resource: { case: "item", value: item } });
    // Then its relatives, so the op's revision is its last.
    for (const relative of this.relatives([prev], [item])) if (relative !== id) this.reemit(relative);
    this.log({ kind: OpKind.UNSPECIFIED, actor: "system", summary: "", events: prev ? diff(prev, item) : [{ kind: "create", itemId: id }] });
    return item;
  }

  /** put on an existing Item. */
  update(id: string, patch: MessageInitShape<typeof ItemSchema>): Item {
    return this.put({ ...patch, id });
  }

  /** Removes an Item outright (not a soft delete) and publishes a deleted Change. */
  remove(id: string): void {
    const prev = this.items.get(id);
    this.revision++;
    this.items.delete(id);
    this.statusEnteredAt.delete(id);
    this.publish({ revision: this.revision, topic: Topic.ITEM, id, deleted: true });
    if (prev?.parentId !== undefined && this.items.has(prev.parentId)) this.reemit(prev.parentId);
  }

  /**
   * Changes a saved view and publishes its Change, off the op log (UpdateView logs an op). A
   * patch that changes the filter, sort or count flag must give the query text too, which ParseQuery then reads
   * back, since the fake cannot print a filter.
   */
  updateView(id: string, patch: MessageInitShape<typeof ViewSchema>): View {
    if (("filter" in patch || "sort" in patch || "showCount" in patch) && patch.query === undefined) {
      throw new Error("updateView: give the query text of the new filter, sort or count flag");
    }
    const prev = this.views.get(id);
    if (!prev) throw new Error(`no view ${id}`);
    const fields = create(ViewSchema, patch) as unknown as Fields;
    const view = clone(ViewSchema, prev);
    for (const k of Object.keys(patch)) (view as unknown as Fields)[k] = fields[k];
    Object.assign(view, { id, revision: ++this.revision, updatedAt: this.stamp() });
    if (patch.query !== undefined) this.defineQuery(view.query, view);
    this.views.set(id, view);
    this.publish({ revision: view.revision, topic: Topic.VIEW, id, resource: { case: "view", value: view } });
    return view;
  }

  /** Deletes a saved view and publishes a deleted Change. */
  removeView(id: string): void {
    this.revision++;
    this.views.delete(id);
    this.publish({ revision: this.revision, topic: Topic.VIEW, id, deleted: true });
  }

  /**
   * Replaces the config, as a reload of an edited file does: one revision
   * and a TOPIC_CONFIG Change carrying it. Unset fields keep their
   * defaults.
   */
  setConfig(patch: MessageInitShape<typeof ConfigSnapshotSchema>): ConfigSnapshot {
    const defaults = defaultConfig(this.config.path);
    const config = create(ConfigSnapshotSchema, patch);
    if (!("path" in patch)) config.path = defaults.path;
    if (!("exists" in patch)) config.exists = true;
    if (!("configVersion" in patch)) config.configVersion = defaults.configVersion;
    // Field by field: a partial [ui] or [backup] keeps the other defaults.
    config.ui = create(UiConfigSchema, { ...DEFAULT_UI, ...(patch.ui && strip(patch.ui)) });
    config.backup = create(BackupConfigSchema, { ...DEFAULT_BACKUP, ...(patch.backup && strip(patch.backup)) });
    this.config = config;
    this.revision++;
    // Stored without the config: it is attached when sent, as the daemon does.
    this.publish({ revision: this.revision, topic: Topic.CONFIG, id: "config" });
    return config;
  }

  /** Fails every open Subscribe stream, as if the connection dropped. */
  /** Sends every open stream a Heartbeat, as the daemon does on a quiet one. */
  heartbeat(): void {
    for (const w of [...this.watchers]) w(msg({ case: "heartbeat", value: {} }));
  }

  dropStreams(code = Code.Unavailable): void {
    for (const w of [...this.watchers]) w(new ConnectError("stream dropped", code));
  }

  /**
   * Replaces the whole state with an older snapshot, as `todo restore` does:
   * the revision goes back to `revision`, the relations are the Items', and
   * the change log, the op log (so the undo stack) and the request ids are
   * emptied.
   */
  restore(revision: bigint, items: Item[]): void {
    this.revision = revision;
    this.items.clear();
    this.relations.clear();
    this.ops.length = 0;
    this.requests.clear();
    this.spools.clear();
    this.statusEnteredAt.clear();
    this.statusEnteredBy.clear();
    this.suggestions.clear();
    this.appliedBy.clear();
    for (const item of items) {
      this.items.set(item.id, this.settle(item));
      for (const r of item.relations) this.relations.set(relationKey(r), clone(RelationSchema, r));
    }
    for (const item of this.items.values()) this.hierarchy(item);
    this.truncateChanges();
  }

  /** Forgets the change log, so older since_revision values need a resync. */
  truncateChanges(): void {
    this.changes.length = 0;
    this.replayFloor = this.revision;
  }

  /** Today in the daemon's zone at `at` (a reference_time), or now. */
  today(at?: Timestamp): number {
    return dayIn(this.timeZone, at === undefined ? this.now().getTime() : epochMs(at));
  }

  /**
   * The seeded views (todo_core::view::defaults()), each with a revision and
   * a TOPIC_VIEW Change, as the store seeds an empty database: no op is
   * written.
   */
  private seedViews(): void {
    const at = this.stamp();
    for (const init of defaultViews()) {
      const view = create(ViewSchema, { ...init, id: ulid(), revision: ++this.revision, createdAt: at, updatedAt: at });
      this.views.set(view.id, view);
      this.publish({ revision: view.revision, topic: Topic.VIEW, id: view.id, resource: { case: "view", value: view } });
    }
  }

  /**
   * An instance's sync state from now on (the daemon's pull loop is not
   * modelled), sent to subscribers as Notice.sync_status.
   */
  setSyncStatus(status: MessageInitShape<typeof SyncStatusSchema>): SyncStatus {
    const s = create(SyncStatusSchema, status);
    this.syncStatus.set(s.instanceId, s);
    for (const w of [...this.watchers]) w(msg({ case: "notice", value: { kind: { case: "syncStatus", value: s } } }));
    return s;
  }

  /**
   * Forgets an instance's status without a notice, as a daemon restarted
   * without it does: only a new stream (its snapshot) leaves it out.
   */
  forgetSyncStatus(instanceId: string): void {
    this.syncStatus.delete(instanceId);
  }

  /**
   * Adds (or replaces) a Suggestion, as sync makes one, and publishes its
   * change; the Item's has_conflict or has_push_failure follows what waits.
   * Patches without an id get one; a missing state is pending.
   */
  putSuggestion(init: MessageInitShape<typeof SuggestionSchema>): Suggestion {
    const s = create(SuggestionSchema, { state: SuggestionState.PENDING, createdAt: this.stamp(), ...init });
    s.id ||= ulid();
    for (const p of s.patches) {
      p.id ||= ulid();
      if (p.state === PatchState.UNSPECIFIED) p.state = PatchState.PENDING;
    }
    this.suggestions.set(s.id, s);
    this.revision++;
    this.publish({ revision: this.revision, topic: Topic.SUGGESTION, id: s.id, resource: { case: "suggestion", value: s } });
    this.flag(s.itemId);
    return s;
  }

  /**
   * What a refine of each Item comes to (ADR-0010): Claude's rationale and
   * patches, or an error the job fails with; unset, nothing to change.
   */
  setRefine(answer: (item: Item) => { rationale: string; patches: MessageInitShape<typeof PatchSchema>[] } | { error: string }): void {
    this.refineAnswer = answer;
  }

  /** Refine: a job, done on the next tick with what setRefine says. */
  private refine(itemIds: readonly string[]): { job: AssistantJob } {
    this.check();
    if (!this.config.assistant?.provider) {
      throw errorInfo(Code.FailedPrecondition, "ASSISTANT_OFF", 'the assistant is off: set [assistant] provider = "claude_code" in the config', {});
    }
    if (!itemIds.length || itemIds.length > 20) throw badRequest("item_ids", "must name 1 to 20 Items");
    const ids = itemIds.map((id) => idOf("item_ids", id));
    for (const id of ids) {
      const item = this.items.get(id);
      if (!item || item.deletedAt !== undefined) throw errorInfo(Code.NotFound, "ITEM_NOT_FOUND", `item ${id} not found`, { item_id: id });
    }
    // One Item a waiting job holds: that job, as the daemon joins it.
    const waiting = ids.length === 1 && [...this.jobs.values()].find((j) => (j.state === AssistantJobState.QUEUED || j.state === AssistantJobState.RUNNING) && j.itemIds.includes(ids[0]!));
    if (waiting) return { job: waiting };
    const job = create(AssistantJobSchema, { id: ulid(), itemIds: ids, state: AssistantJobState.QUEUED, model: this.config.assistant.model, createdAt: this.stamp() });
    this.jobs.set(job.id, job);
    setTimeout(() => {
      job.state = AssistantJobState.RUNNING;
      for (const id of ids) {
        const item = this.items.get(id);
        if (!item || item.deletedAt !== undefined) continue;
        const out = this.refineAnswer(item);
        if ("error" in out) {
          job.state = AssistantJobState.FAILED;
          job.error = out.error;
          return;
        }
        if (!out.patches.length) continue;
        const s = this.putSuggestion({ itemId: id, kind: SuggestionKind.REFINE, source: SuggestionSource.CLAUDE, rationale: out.rationale, patches: out.patches });
        job.suggestionIds.push(s.id);
      }
      job.state = AssistantJobState.DONE;
    }, 0);
    return { job };
  }

  /**
   * Promote (`key` undefined: a new issue in `project`, else promote_default's)
   * or attach (to `key`, which must be one setIssue made): the Item takes the
   * issue's title and key, pinned, as the daemon links it (design 5.7, 3.3).
   */
  private link(itemId: string, key: string | undefined, project: string | undefined) {
    this.check();
    const instance = this.config.instances[0];
    if (!instance) throw errorInfo(Code.FailedPrecondition, "NO_INSTANCE", "no [[instances]] are configured to sync", {});
    if (!instance.push) throw errorInfo(Code.FailedPrecondition, "PUSH_OFF", `${instance.id} is read-only (push = false)`, {});
    const id = idOf("item_id", itemId);
    const item = this.items.get(id);
    if (!item || item.deletedAt !== undefined) throw errorInfo(Code.NotFound, "ITEM_NOT_FOUND", `item ${id} not found`, { item_id: id });
    if (item.remoteLink) throw errorInfo(Code.FailedPrecondition, "ALREADY_LINKED", `already linked to ${item.remoteLink.key}`, { key: item.remoteLink.key });
    let title = item.title;
    let made: string;
    if (key === undefined) {
      const p = project?.trim() || instance.promoteProject;
      if (!p) throw errorInfo(Code.FailedPrecondition, "NO_PROMOTE_TARGET", "no project given, and no promote_default", {});
      made = `${p.toUpperCase()}-${++this.issueSeq}`;
    } else {
      const issue = this.issues.get(key);
      if (!issue) throw errorInfo(Code.NotFound, "ISSUE_NOT_FOUND", "Jira has no such issue you can read", {});
      made = key;
      title = issue.title;
    }
    const remoteLink = { trackerKind: "jira", instanceId: instance.id, remoteId: String(this.issueSeq), key: made, url: `https://jira.example.com/browse/${made}`, pinned: true, linkState: LinkState.OK };
    const lost = title !== item.title;
    const next = this.put({ id, title, remoteLink, isMine: true });
    let suggestionId = "";
    if (lost) {
      const s = this.putSuggestion({
        itemId: id,
        kind: SuggestionKind.LINK_CONFLICT,
        source: SuggestionSource.SYNC,
        rationale: `${made}'s values replaced this todo's when it was linked`,
        patches: [{ field: ItemField.TITLE, expectedOld: { kind: { case: "stringValue", value: title } }, new: { kind: { case: "stringValue", value: item.title } } }],
      });
      suggestionId = s.id;
    }
    return { item: next, result: { opId: ulid(), revision: this.revision }, suggestionId };
  }

  /** An issue attach can link to, by key, with its title. */
  setIssue(key: string, title: string): void {
    this.issues.set(key, { title });
  }

  /** What ListOutbox answers from now on. */
  setOutbox(entries: readonly MessageInitShape<typeof OutboxEntrySchema>[]): void {
    this.outbox = entries.map((e) => create(OutboxEntrySchema, e));
  }

  /** The Item's has_conflict and has_push_failure from what waits; a change of either is published. */
  private flag(itemId: string): void {
    const item = this.items.get(itemId);
    if (!item) return;
    const kinds = [...this.suggestions.values()].filter((s) => s.itemId === itemId && waits(s)).map((s) => s.kind);
    const conflict = kinds.includes(SuggestionKind.SYNC_CONFLICT);
    const failure = kinds.includes(SuggestionKind.PUSH_FAILED);
    if (item.hasConflict === conflict && item.hasPushFailure === failure) return;
    this.put({ id: itemId, hasConflict: conflict, hasPushFailure: failure });
  }

  /**
   * ApplySuggestionPatches as the daemon runs it: one op by the person for
   * every patch that still holds expected_old (a status pair as one
   * SetStatus), through the guard a direct edit goes through; the others
   * are marked stale and skipped, as are patches no longer pending.
   */
  private applyPatches(requestId: string, patchIds: readonly string[], confirmNotMine: boolean) {
    this.check();
    const key = requestIdOf(requestId);
    if (patchIds.length < 1 || patchIds.length > MAX_BATCH) throw badRequest("patch_ids", `must hold 1 to ${MAX_BATCH} ids`);
    // Patch ids are ULIDs, as the daemon checks them before looking them up.
    const named = [...new Set(patchIds.map((id, i) => {
      const canonical = canonicalUlid(id);
      if (canonical === null) throw badRequest(`patch_ids[${i}]`, "must be a ULID");
      return canonical;
    }))];
    const found = named.map((id) => {
      for (const s of this.suggestions.values()) {
        const p = s.patches.find((x) => x.id === id);
        if (p) return { s, p };
      }
      throw errorInfo(Code.NotFound, "PATCH_NOT_FOUND", `no suggestion has the patch ${id}`, { patch_id: id });
    });
    const recorded = this.recorded("ApplySuggestionPatches", key);
    if (recorded) {
      // As the daemon's replay: what that op applied, not what the patches hold now.
      const applied = new Set((this.appliedBy.get(recorded.op.id) ?? []).map((p) => p.id));
      const touched = itemIdsOf(recorded.op.events);
      return {
        result: recorded.result,
        items: touched.map((id) => this.present(this.get(id), this.today())),
        suggestions: [...new Set(found.map((f) => f.s))],
        skippedPatchIds: named.filter((id) => !applied.has(id)),
      };
    }
    const status = (f: ItemField) => f === ItemField.STATUS_CATEGORY || f === ItemField.RESOLUTION_KIND;
    // Each named status patch brings the other half of its pair.
    const chosen = [...found];
    for (const { s, p } of found) {
      if (!status(p.field) || p.state !== PatchState.PENDING) continue;
      for (const q of s.patches) if (status(q.field) && q.state === PatchState.PENDING && !chosen.some((c) => c.p === q)) chosen.push({ s, p: q });
    }
    const planned: Planned[] = [];
    const applied: Patch[] = [];
    const stale: Patch[] = [];
    for (const itemId of new Set(chosen.map((c) => c.s.itemId))) {
      const stored = this.items.get(itemId);
      if (!stored || stored.deletedAt !== undefined) continue;
      // As apply_tx: field patches one at a time, then each Suggestion's
      // status pair, each checked on the Item as the earlier ones left it.
      let item = stored;
      const holds = (p: Patch) => matches(localOf(p.field), (item as unknown as Fields)[localOf(p.field)], valueOfField(p.field, p.expectedOld));
      const mine = chosen.filter((c) => c.s.itemId === itemId && c.p.state === PatchState.PENDING);
      const values: Fields = {};
      const paths: string[] = [];
      for (const c of mine.filter((c) => !status(c.p.field))) {
        if (!holds(c.p)) {
          stale.push(c.p);
          continue;
        }
        const path = PATH_OF_NUMBER.get(c.p.field)!;
        const value = valueOfField(c.p.field, c.p.new);
        if (!paths.includes(path)) paths.push(path);
        values[path] = value;
        item = { ...item, [localOf(c.p.field)]: value } as Item;
        applied.push(c.p);
      }
      if (paths.length) {
        planned.push({ kind: "update", id: itemId, expectedRevision: undefined, prefix: "", idPath: "patch_ids", batched: false, confirmNotMine, mask: { paths, values } });
      }
      for (const s of new Set(mine.filter((c) => status(c.p.field)).map((c) => c.s))) {
        const pair = mine.filter((c) => c.s === s && status(c.p.field));
        if (!pair.every((c) => holds(c.p))) {
          stale.push(...pair.map((c) => c.p));
          continue;
        }
        const category = valueOfField(ItemField.STATUS_CATEGORY, pair.find((c) => c.p.field === ItemField.STATUS_CATEGORY)?.p.new) as StatusCategory | undefined;
        const kind = valueOfField(ItemField.RESOLUTION_KIND, pair.find((c) => c.p.field === ItemField.RESOLUTION_KIND)?.p.new) as ResolutionKind | undefined;
        const target = statusFor(item, category ?? item.statusCategory, kind);
        planned.push({ kind: "setStatus", id: itemId, expectedRevision: undefined, prefix: "", idPath: "patch_ids", batched: false, confirmNotMine, status: target });
        item = { ...item, ...withStatus(item, target), status: target } as Item;
        applied.push(...pair.map((c) => c.p));
      }
    }
    // An apply never generates a series' next instance (apply_tx runs no recurrence pass).
    const done = planned.length
      ? this.mutate("ApplySuggestionPatches", OpKind.APPLY_SUGGESTION, key, planned, { generate: false })
      : undefined;
    const opResult = done?.result ?? this.record("ApplySuggestionPatches", key, "", this.log({ kind: OpKind.APPLY_SUGGESTION, actor: "user", summary: "apply nothing", events: [] }));
    for (const p of applied) p.state = PatchState.APPLIED;
    for (const p of stale) p.state = PatchState.STALE;
    const recordedOp = this.requests.get(key)?.op;
    if (recordedOp) this.appliedBy.set(recordedOp.id, applied);
    const touched = [...new Set(chosen.map((c) => c.s))].filter((s) => s.patches.some((p) => applied.includes(p) || stale.includes(p)));
    for (const s of touched) {
      s.state = settledState(s);
      this.revision++;
      this.publish({ revision: this.revision, topic: Topic.SUGGESTION, id: s.id, resource: { case: "suggestion", value: s } });
    }
    for (const id of new Set(chosen.map((c) => c.s.itemId))) this.flag(id);
    // The flags and the Suggestion changes are the op's: its result names the last revision.
    const result = { ...opResult, revision: this.revision };
    const req = this.requests.get(key);
    if (req) req.result = result;
    return {
      result,
      items: (done?.items ?? []).map((i) => this.present(this.get(i.id), this.today())),
      suggestions: [...new Set(chosen.map((c) => c.s))],
      skippedPatchIds: named.filter((id) => !applied.some((p) => p.id === id)),
    };
  }

  /** RejectSuggestion: every pending patch rejected; never an undo stack entry. */
  private rejectSuggestion(requestId: string, given: string) {
    this.check();
    const key = requestIdOf(requestId);
    const id = canonicalUlid(given);
    if (id === null) throw badRequest("id", "must be a ULID");
    const s = this.suggestions.get(id);
    if (!s) throw errorInfo(Code.NotFound, "SUGGESTION_NOT_FOUND", `suggestion ${id} not found`, { suggestion_id: id });
    const recorded = this.recorded("RejectSuggestion", key, id);
    if (recorded) return { suggestion: s, result: recorded.result };
    if (s.patches.some((p) => p.state === PatchState.PENDING)) {
      for (const p of s.patches) if (p.state === PatchState.PENDING) p.state = PatchState.REJECTED;
      s.state = settledState(s);
      this.revision++;
      this.publish({ revision: this.revision, topic: Topic.SUGGESTION, id: s.id, resource: { case: "suggestion", value: s } });
      this.flag(s.itemId);
    }
    const op = this.log({ kind: OpKind.REJECT_SUGGESTION, actor: "system", summary: "reject suggestion", events: [] });
    return { suggestion: s, result: this.record("RejectSuggestion", key, id, op) };
  }

  /** What GetActivityReport answers from now on; the revision and today are the fake's. */
  setActivityReport(report: MessageInitShape<typeof GetActivityReportResponseSchema>): void {
    this.activityReport = report;
  }

  /**
   * Query text a test parses, as the daemon would read it. ParseQuery knows
   * the default views' text and what {@link defineQuery} adds; any other
   * text is one error over all of it, since the grammar is the daemon's.
   */
  defineQuery(text: string, query: { filter?: MessageInitShape<typeof FilterSchema>; sort?: MessageInitShape<typeof SortKeySchema>[]; showCount?: boolean }): void {
    this.queries.set(text, { filter: create(FilterSchema, query.filter ?? {}), sort: (query.sort ?? []).map((k) => create(SortKeySchema, k)), showCount: query.showCount ?? true });
  }

  /** ParseQuery from {@link queries}. */
  private parseQuery(req: ParseQueryRequest): MessageInitShape<typeof ParseQueryResponseSchema> {
    this.check();
    if (new TextEncoder().encode(req.text).length > 4096) throw badRequest("text", "is longer than 4096 bytes");
    const known = this.queries.get(req.text);
    // As the daemon, the count flag is answered even beside errors.
    if (!known) return { errors: [{ start: 0, end: [...req.text].length, message: "fake: unknown query" }], showCount: !/(^|\s)count:off(\s|$)/i.test(req.text) };
    const today = this.referenceDay(req.referenceTime);
    return {
      filter: known.filter,
      sort: known.sort,
      showCount: known.showCount,
      ...(req.count && { count: this.passing(known.filter, today).length }),
    };
  }

  // --- view edits (design 2.4), as the store's view.rs: ops for their
  // request_id, never undo stack entries, positions kept dense ---

  private createView(req: CreateViewRequest): MessageInitShape<typeof CreateViewResponseSchema> {
    this.check();
    // The daemon's order: request_id, the filter and sort, the replay, then the store's checks.
    const requestId = requestIdOf(req.requestId);
    const filter = req.filter ?? create(FilterSchema);
    checkViewFilter("", filter, req.sort);
    const recorded = this.recorded("CreateView", requestId);
    if (recorded) return { ...this.replayedView(recorded.id), result: recorded.result };
    const name = viewName(req.name, "name");
    this.nameFree(name);
    const count = this.views.size;
    const position = Math.min(req.position ?? count, count);
    const moved = this.shiftViews(position, count - 1, 1);
    const at = this.stamp();
    const view = create(ViewSchema, { id: ulid(), name, filter, sort: req.sort, showCount: req.showCount, position, revision: ++this.revision, createdAt: at, updatedAt: at });
    this.storeView(view, "unknown");
    const op = this.log({ kind: OpKind.CREATE_VIEW, actor: "user", summary: `create view ${JSON.stringify(name)}`, events: [] });
    return { view, moved, result: this.record("CreateView", requestId, view.id, op) };
  }

  private editView(req: UpdateViewRequest): MessageInitShape<typeof UpdateViewResponseSchema> {
    this.check();
    const requestId = requestIdOf(req.requestId);
    if (!req.view) throw badRequest("view", "is required");
    const id = canonicalUlid(req.view.id);
    if (id === null) throw badRequest("view.id", "must be a ULID");
    const paths = req.updateMask?.paths ?? [];
    if (!paths.length) throw badRequest("update_mask", "names no field");
    const seen = new Set<string>();
    for (const [i, path] of paths.entries()) {
      if (!["name", "filter", "sort", "show_count"].includes(path)) {
        throw badRequest(`update_mask.paths[${i}]`, `${JSON.stringify(path)} cannot be updated: use name, filter, sort or show_count`);
      }
      if (seen.has(path)) throw badRequest(`update_mask.paths[${i}]`, `${path} is named twice`);
      seen.add(path);
    }
    // Only what the mask names is read.
    checkViewFilter("view.", seen.has("filter") ? (req.view.filter ?? create(FilterSchema)) : undefined, seen.has("sort") ? req.view.sort : undefined);
    const name = seen.has("name") ? viewName(req.view.name, "view.name") : undefined;
    const recorded = this.recorded("UpdateView", requestId, id);
    if (recorded) return { ...this.replayedView(recorded.id), result: recorded.result };
    const prev = this.views.get(id);
    if (!prev) throw errorInfo(Code.NotFound, "VIEW_NOT_FOUND", `view ${id} not found`, { view_id: id });
    if (req.expectedRevision !== undefined && req.expectedRevision !== prev.revision) {
      throw errorInfo(Code.Aborted, "REVISION_MISMATCH", `view ${id}: revision conflict: expected ${req.expectedRevision}, found ${prev.revision}`, {
        view_id: id,
        expected_revision: String(req.expectedRevision),
        actual_revision: String(prev.revision),
      });
    }
    const next = clone(ViewSchema, prev);
    if (name !== undefined) {
      this.nameFree(name, id);
      next.name = name;
    }
    if (seen.has("filter")) next.filter = req.view.filter ?? create(FilterSchema);
    if (seen.has("sort")) next.sort = req.view.sort;
    if (seen.has("show_count")) next.showCount = req.view.showCount;
    if (!equals(ViewSchema, next, prev)) {
      Object.assign(next, { revision: ++this.revision, updatedAt: this.stamp() });
      this.storeView(next, "unknown");
    }
    const op = this.log({ kind: OpKind.UPDATE_VIEW, actor: "user", summary: `edit view ${JSON.stringify(next.name)}`, events: [] });
    return { view: next, result: this.record("UpdateView", requestId, id, op) };
  }

  private deleteView(req: DeleteViewRequest): MessageInitShape<typeof DeleteViewResponseSchema> {
    this.check();
    const requestId = requestIdOf(req.requestId);
    const id = canonicalUlid(req.id);
    if (id === null) throw badRequest("id", "must be a ULID");
    const recorded = this.recorded("DeleteView", requestId, id);
    if (recorded) return { result: recorded.result };
    const view = this.views.get(id);
    if (!view) throw errorInfo(Code.NotFound, "VIEW_NOT_FOUND", `view ${id} not found`, { view_id: id });
    this.views.delete(id);
    this.publish({ revision: ++this.revision, topic: Topic.VIEW, id, deleted: true });
    const moved = this.shiftViews(view.position + 1, this.views.size, -1);
    const op = this.log({ kind: OpKind.DELETE_VIEW, actor: "user", summary: `delete view ${JSON.stringify(view.name)}`, events: [] });
    return { moved, result: this.record("DeleteView", requestId, id, op) };
  }

  private moveView(req: MoveViewRequest): MessageInitShape<typeof MoveViewResponseSchema> {
    this.check();
    const requestId = requestIdOf(req.requestId);
    const id = canonicalUlid(req.id);
    if (id === null) throw badRequest("id", "must be a ULID");
    const recorded = this.recorded("MoveView", requestId, id);
    if (recorded) return { ...this.replayedView(recorded.id), result: recorded.result };
    const view = this.views.get(id);
    if (!view) throw errorInfo(Code.NotFound, "VIEW_NOT_FOUND", `view ${id} not found`, { view_id: id });
    const from = view.position;
    const to = Math.min(req.position, this.views.size - 1);
    const summary = `move view ${JSON.stringify(view.name)}`;
    if (to === from) {
      const op = this.log({ kind: OpKind.MOVE_VIEW, actor: "user", summary, events: [] });
      return { view, result: this.record("MoveView", requestId, id, op) };
    }
    const moved = to < from ? this.shiftViews(to, from - 1, 1, id) : this.shiftViews(from + 1, to, -1, id);
    const next = create(ViewSchema, { ...view, position: to, revision: ++this.revision, updatedAt: this.stamp() });
    this.storeView(next, "unknown");
    moved.push(next);
    const op = this.log({ kind: OpKind.MOVE_VIEW, actor: "user", summary, events: [] });
    return { view: next, moved, result: this.record("MoveView", requestId, id, op) };
  }

  /** The views with a position from `lo` to `hi`, moved by `by`, each with a revision and a change. */
  private shiftViews(lo: number, hi: number, by: number, except?: string): View[] {
    const moved: View[] = [];
    for (const v of this.tabOrder()) {
      if (v.id === except || v.position < lo || v.position > hi) continue;
      const next = create(ViewSchema, { ...v, position: v.position + by, revision: ++this.revision });
      this.storeView(next, "keep");
      moved.push(next);
    }
    return moved;
  }

  /**
   * Stores and publishes a view. Its query text is the daemon's printer's,
   * which the fake does not have: "keep" leaves it, as a move does; "unknown"
   * keeps it only if the fake knows the text names this filter, sort and
   * count flag (a test gives it with defineQuery).
   */
  private storeView(view: View, query: "keep" | "unknown"): void {
    if (query === "unknown") {
      const known = [...this.queries].find(([, q]) => equals(FilterSchema, q.filter, view.filter ?? create(FilterSchema)) && q.showCount === view.showCount && q.sort.length === view.sort.length && q.sort.every((k, i) => equals(SortKeySchema, k, view.sort[i]!)));
      view.query = known?.[0] ?? "";
    }
    this.views.set(view.id, view);
    this.publish({ revision: view.revision, topic: Topic.VIEW, id: view.id, resource: { case: "view", value: view } });
  }

  /** Fails with VIEW_NAME_TAKEN when a view but `except` has `name`, ignoring case in every script, as Rust's to_lowercase. */
  private nameFree(name: string, except?: string): void {
    const lower = name.toLowerCase();
    if ([...this.views.values()].some((v) => v.id !== except && v.name.toLowerCase() === lower)) {
      throw errorInfo(Code.AlreadyExists, "VIEW_NAME_TAKEN", `a view named ${JSON.stringify(name)} exists`, { name });
    }
  }

  /** The view a replayed request made, as it is now: none once deleted. */
  private replayedView(id: string): { view?: View } {
    const view = this.views.get(id);
    return view ? { view } : {};
  }

  /** Every view in tab order. */
  private tabOrder(): View[] {
    return [...this.views.values()].sort((a, b) => a.position - b.position || cmp(a.id, b.id));
  }

  /**
   * A write time: now, or just after the last one when the clock has not
   * moved on since (it reads in milliseconds).
   */
  private stamp(): Timestamp {
    const ns = BigInt(this.now().getTime()) * 1_000_000n;
    this.lastWrite = ns > this.lastWrite ? ns : this.lastWrite + 1n;
    const seconds = this.lastWrite / NANOS - (this.lastWrite % NANOS < 0n ? 1n : 0n);
    return create(TimestampSchema, { seconds, nanos: Number(this.lastWrite - seconds * NANOS) });
  }

  /** Today for a request's reference_time, as the daemon reads it (INVALID_ARGUMENT on "reference_time"). */
  private referenceDay(t: Timestamp | undefined): number {
    if (t === undefined) return this.today();
    const ns = t.seconds * NANOS + BigInt(t.nanos);
    if (t.nanos < 0 || t.nanos >= 1e9) throw badRequest("reference_time", `nanos ${t.nanos} out of range`);
    if (ns < -(2n ** 63n) || ns >= 2n ** 63n) {
      throw badRequest("reference_time", "must be between 1677-09-21T00:12:44Z and 2262-04-11T23:47:16Z");
    }
    return this.today(t);
  }

  /** Derives a stored Item's effective_priority and is_overdue (status is kept with its triple). */
  private settle(item: Item): Item {
    item.effectivePriority = effectivePriority(item);
    item.isOverdue = isOverdue(item, this.today());
    return item;
  }

  /**
   * Derives a stored Item's subtask counts (direct subtasks, not deleted or
   * cancelled) and parent title from the stored Items, as hierarchy.rs
   * attaches them.
   */
  private hierarchy(item: Item): Item {
    let total = 0;
    let done = 0;
    for (const child of this.items.values()) {
      if (child.parentId !== item.id || child.deletedAt !== undefined || child.status === Status.CANCELLED) continue;
      total++;
      if (child.status === Status.DONE) done++;
    }
    item.subtasksTotal = total;
    item.subtasksDone = done;
    const parent = item.parentId === undefined ? undefined : this.items.get(item.parentId);
    if (parent) item.parentTitle = parent.title;
    else delete item.parentTitle;
    return item;
  }

  /**
   * The ids whose derived hierarchy fields change when Items go from
   * `before` to `after` (pairwise; undefined for a new Item), without
   * changing them: the old and new parent of each whose contribution
   * changed, and the subtasks of each retitled one (Batch::fan_out). In
   * its order: every parent first, the new one before the old, then the
   * subtasks, the stored ones by creation and then those `after` moves in.
   */
  private relatives(before: readonly (Item | undefined)[], after: readonly Item[]): string[] {
    const out = new Set<string>();
    const retitled: string[] = [];
    after.forEach((item, i) => {
      const prev = before[i];
      if (contribution(prev) !== contribution(item)) {
        for (const p of [item.parentId, prev?.parentId]) if (p !== undefined) out.add(p);
      }
      if (prev && prev.title !== item.title) retitled.push(item.id);
    });
    const now = new Map(after.map((item) => [item.id, item]));
    for (const id of retitled) {
      const stored = [...this.items.values()].filter((c) => c.parentId === id).sort(byCreation);
      for (const child of stored) if ((now.get(child.id) ?? child).parentId === id) out.add(child.id);
      for (const child of after) if (child.parentId === id) out.add(child.id);
    }
    return [...out];
  }

  /** Gives a stored Item a revision and publishes it with its hierarchy fields fresh, its updated_at kept. */
  private reemit(id: string): void {
    const item = this.items.get(id);
    if (!item) return;
    const next = this.hierarchy({ ...item, revision: ++this.revision });
    this.items.set(id, next);
    this.publish({ revision: next.revision, topic: Topic.ITEM, id, resource: { case: "item", value: next } });
  }

  /** `item` as read on day `today`: is_overdue is for that day. */
  private present(item: Item, today: number): Item {
    const overdue = isOverdue(item, today);
    return item.isOverdue === overdue ? item : { ...item, isOverdue: overdue };
  }

  /** The stored Items `filter` passes. */
  private passing(filter: Filter, today: number): Item[] {
    const resolved = resolveFilter(filter, today);
    if (!resolved) throw new ConnectError("a stored filter does not resolve", Code.Internal);
    return [...this.items.values()].filter((i) => passes(i, resolved));
  }

  /**
   * ListItems, with the daemon's errors in its order: reference_time, the
   * filter's first INVALID, the sort, the filter's UNIMPLEMENTED, page_size,
   * view_id; then the page token, the view (VIEW_NOT_FOUND), and the token's
   * fit to the effective filter and sort.
   */
  private list(req: ListItemsRequest): MessageInitShape<typeof ListItemsResponseSchema> {
    const requested = this.referenceDay(req.referenceTime);
    const filter = req.filter ?? create(FilterSchema);
    const filterProblem = checkFilter(filter, requested);
    if (filterProblem?.kind === "invalid") throw problemError(filterProblem);
    const sortProblem = checkSort(req.sort);
    if (sortProblem) throw problemError(sortProblem);
    if (filterProblem) throw problemError(filterProblem);
    if (req.pageSize < 0) throw badRequest("page_size", "must not be negative");
    const viewId = req.viewId ? canonicalUlid(req.viewId) : undefined;
    if (viewId === null) throw badRequest("view_id", "must be a ULID");
    const cursor = req.pageToken === "" ? undefined : decodeCursor(req.pageToken);
    if (cursor === undefined && req.pageToken !== "") throw badRequest("page_token", "invalid page token");
    let effective = { filter, sort: req.sort };
    if (viewId !== undefined) {
      const view = this.views.get(viewId);
      if (!view) throw errorInfo(Code.NotFound, "VIEW_NOT_FOUND", `view ${viewId} not found`, { view_id: viewId });
      effective = narrow(view, filter, req.sort);
    }
    const keys = sortKeys(effective.sort);
    const bound = fingerprint(effective.filter, keys);
    if (cursor && (cursor.fingerprint !== bound || cursor.after.length !== keys.length)) {
      throw badRequest("page_token", "invalid page token");
    }
    // A later page keeps the first page's today.
    const today = cursor?.today ?? requested;
    const resolved = resolveFilter(effective.filter, today);
    if (!resolved) throw badRequest("page_token", "invalid page token");
    const rows = [...this.items.values()]
      .filter((i) => passes(i, resolved))
      .map((item) => ({ item, place: sortTuple(item, keys, today) }))
      .sort((a, b) => compareTuples(a.place, b.place, keys));
    const after = cursor ? rows.filter((r) => compareTuples(r.place, cursor.after, keys) > 0) : rows;
    const size = Math.min(req.pageSize === 0 ? DEFAULT_PAGE_SIZE : req.pageSize, this.maxPageSize);
    const page = after.slice(0, size);
    const last = page.at(-1);
    return {
      items: page.map((r) => this.present(r.item, today)),
      nextPageToken: last && after.length > page.length ? encodeCursor({ fingerprint: bound, today, after: last.place }) : "",
      revision: this.revision,
    };
  }

  /** GetStatusCounts: each count, and each counted view's, passes the implicit rules, as a listing does. */
  private statusCounts(req: GetStatusCountsRequest): MessageInitShape<typeof GetStatusCountsResponseSchema> {
    const today = this.referenceDay(req.referenceTime);
    const count = (filter: Filter) => this.passing(filter, today).length;
    const one = (target: ItemField, op: Operator, operand: MessageInitShape<typeof OperandSchema>) =>
      create(FilterSchema, { allOf: [{ kind: { case: "predicate", value: { target, op, operands: [operand] } } }] });
    const inbox = one(ItemField.STATUS, Operator.IN, { kind: { case: "value", value: { kind: { case: "status", value: Status.INBOX } } } });
    const onToday = { kind: { case: "relativeDate", value: { anchor: RelativeDate_Anchor.TODAY } } } as const;
    return {
      counts: {
        inbox: count(inbox),
        due: count(one(ItemField.DUE_DATE, Operator.LE, onToday)),
        overdue: count(one(ItemField.DUE_DATE, Operator.LT, onToday)),
      },
      views: req.includeViews
        ? this.tabOrder()
            .filter((v) => v.showCount)
            .map((v) => ({ viewId: v.id, count: count(narrow(v, undefined, []).filter) }))
        : [],
      revision: this.revision,
      today: protoDate(today),
    };
  }

  /** ParseDate: the capture date grammar, with an optional leading "^". */
  private parseDate(req: ParseDateRequest): MessageInitShape<typeof ParseDateResponseSchema> {
    if (overCaptureLimit(req.text)) throw badRequest("text", TOO_LONG);
    const today = this.referenceDay(req.referenceTime);
    const typed = rustTrim(req.text);
    const parsed = parseDateExpr(typed.startsWith("^") ? typed.slice(1) : typed, today);
    if ("day" in parsed) return { date: protoDate(parsed.day), warning: "" };
    // As ParseCapture words a ^ token that is not a date.
    return { warning: typed === "" ? parsed.reason : `${typed}: ${parsed.reason}` };
  }

  /** ParseCapture: the capture syntax on the reference day; blank text gives an empty draft. */
  private parseCapture(req: ParseCaptureRequest): { draft: CaptureDraft } {
    if (overCaptureLimit(req.text)) throw badRequest("text", TOO_LONG);
    return { draft: parseCaptureText(req.text, this.referenceDay(req.referenceTime)) };
  }

  /**
   * CaptureItem, with the daemon's checks in its order. The text is parsed
   * on the reference day, which is the new Item's created_at; a replay (by
   * request_id, then by spool_id) returns the original result, the Item's
   * current state and the text parsed on the day it was created.
   */
  private capture(req: CaptureItemRequest): { item: Item; result: MutationResult; draft: CaptureDraft } {
    this.check();
    const requestId = requestIdOf(req.requestId);
    if (overCaptureLimit(req.text)) throw badRequest("text", TOO_LONG);
    if (rustTrim(req.text) === "") throw badRequest("text", "must not be empty");
    checkCreatedVia(req.createdVia);
    if (req.referenceTime !== undefined) this.referenceDay(req.referenceTime);
    const parentId = req.parentId === undefined ? undefined : canonicalUlid(req.parentId);
    if (parentId === null) throw badRequest("parent_id", "must be a ULID");
    const spoolId = req.spoolId ? canonicalUlid(req.spoolId) : undefined;
    if (spoolId === null) throw badRequest("spool_id", "must be a ULID");
    const recorded = this.recorded("CaptureItem", requestId) ?? (spoolId === undefined ? undefined : this.spools.get(spoolId));
    if (recorded) {
      const item = this.present(this.get(recorded.id), this.today());
      return { item, result: recorded.result, draft: parseCaptureText(req.text, this.today(item.createdAt)) };
    }
    const problem = parentId === undefined ? undefined : this.batch().unusableParent(parentId);
    if (problem) throw badRequest("parent_id", problem);
    // The reference time is the new Item's created_at, as a spool import's capture time is.
    const now = req.referenceTime ?? this.stamp();
    const day = this.today(now);
    const draft = parseCaptureText(req.text, day);
    const item = create(ItemSchema, {
      id: this.newId(),
      title: draft.title,
      createdVia: req.createdVia,
      createdAt: now,
      updatedAt: now,
      itemType: "task",
      priority: draft.priority,
      energy: draft.energy,
      labels: draft.labels,
      ...withStatus(INBOX_TRIPLE, draft.status),
      status: draft.status,
      ...(draft.dueDate !== undefined && { dueDate: draft.dueDate }),
      ...(draft.waitingOn !== undefined && { waiting: { on: draft.waitingOn, reason: "", since: protoDate(day) } }),
      ...(parentId !== undefined && { parentId }),
    });
    if (draft.recurrence !== undefined) {
      // A rule on an Item without recurrence (design 3.8): a new series,
      // scheduled on the draft's first date, which also becomes the due date
      // when no ^ token set one. The capture is never deferred.
      const first = draft.recurrenceFirst;
      if (item.dueDate === undefined && first !== undefined) item.dueDate = clone(DateSchema, first);
      item.recurrence = create(ItemRecurrenceSchema, {
        seriesId: ulid(),
        rule: clone(RecurrenceRuleSchema, draft.recurrence),
        ...(first !== undefined && { occurrenceDate: clone(DateSchema, first) }),
      });
    }
    this.statusEnteredAt.set(item.id, nanosOf(now));
    // A subtask re-emits its parent first, so the capture's revision is the op's last.
    this.items.set(item.id, this.settle(item));
    if (parentId !== undefined) this.reemit(parentId);
    item.revision = ++this.revision;
    this.hierarchy(item);
    const op = this.log({ kind: OpKind.CAPTURE, actor: "user", summary: `capture ${quoted(item.title)}`, events: [{ kind: "create", itemId: item.id }] });
    this.publish({ revision: item.revision, topic: Topic.ITEM, id: item.id, resource: { case: "item", value: item } });
    const result = this.record("CaptureItem", requestId, item.id, op);
    if (spoolId !== undefined) this.spools.set(spoolId, this.requests.get(requestId)!);
    return { item, result, draft };
  }

  /** A single-item RPC: checks `requestId`, converts the request with `plan` and runs it as one op of `kind`. */
  private single(rpc: string, kind: OpKind, requestId: string, plan: () => Planned): MutationResponse {
    this.check();
    const key = requestIdOf(requestId);
    const { items, result, generated } = this.mutate(rpc, kind, key, [plan()]);
    return { item: items[0]!, result, generated };
  }

  /**
   * ParseRecurrence, with the daemon's checks in its order (the text's
   * length, reference_time, the anchor): the rule grammar of an `@` token,
   * resolved against the anchor, else the reference day.
   */
  private parseRecurrence(req: ParseRecurrenceRequest): ParseRecurrenceResponse {
    if (overCaptureLimit(req.text)) throw badRequest("text", TOO_LONG);
    const today = this.referenceDay(req.referenceTime);
    const anchor = req.anchor === undefined ? undefined : dateArg("anchor", req.anchor);
    return parseRecurrenceText(req.text, anchor, today);
  }

  /**
   * Runs the converted `mutations` in order as one op, all or nothing, in
   * the store's order: a replay returns the original result with the Items'
   * current state; otherwise each Item must exist, be at its
   * expected_revision (as it was before the op) and, unless the mutation is
   * a delete or undelete, not be deleted. A delete takes the live
   * descendants along, an undelete the descendants its delete took; no
   * Item the op undeleted may end it live under a deleted parent. Then the
   * recurrence post-pass ({@link generate}) creates the next instances.
   * A replay also answers the instances the op created, as they are now.
   */
  private mutate(
    rpc: string,
    kind: OpKind,
    requestId: string,
    planned: Planned[],
    opts: { generate?: boolean } = {},
  ): { items: Item[]; result: MutationResult; generated: Item[] } {
    const today = this.today();
    // A single-item op names its Item, so the same request id on another Item
    // is a different request; a batch is matched by its RPC alone.
    const recorded = this.recorded(rpc, requestId, kind === OpKind.BATCH ? undefined : planned[0]!.id);
    if (recorded) {
      const created = recorded.op.events.flatMap((e) => (e.kind === "create" ? [this.present(this.get(e.itemId), today)] : []));
      return { items: planned.map((p) => this.present(this.get(p.id), today)), result: recorded.result, generated: created };
    }
    const now = this.stamp();
    const batch = this.batch();
    // For the summary: the Items a cascade took along that no mutation names.
    const named = new Set(planned.map((p) => p.id));
    const subtasks = new Set<string>();
    // Each undelete with the mutation that made it, for the parent rule,
    // which holds for the op as a whole.
    const undeleted: [Planned, string][] = [];
    const slots: number[] = [];
    for (const p of planned) {
      const slot = batch.load(p.id);
      const item = batch.items[slot]!;
      if (p.expectedRevision !== undefined && p.expectedRevision !== item.revision) {
        throw errorInfo(
          Code.Aborted,
          "REVISION_MISMATCH",
          `item ${p.id}: revision conflict: expected ${p.expectedRevision}, found ${item.revision}`,
          { item_id: p.id, expected_revision: String(p.expectedRevision), actual_revision: String(item.revision) },
        );
      }
      if (item.deletedAt !== undefined && p.kind !== "delete" && p.kind !== "undelete") {
        throw errorInfo(Code.FailedPrecondition, "ITEM_DELETED", inBatch(p, `item ${p.id} is deleted`), { item_id: p.id });
      }
      // Read before the undelete replaces it.
      const deletedBy = p.kind === "undelete" ? batch.deletedBy(p.id) : undefined;
      const { next, changes } = applyPatch(item, patchOf(p, item, now));
      // On a linked Item a person changes Overlay fields, and the pushed ones
      // while its instance pushes and its link is whole (ADR-0006, ADR-0007),
      // as todo_core's apply refuses.
      const pushes =
        // A fixture's link without a state is whole, as the daemon writes it.
        [LinkState.OK, LinkState.UNSPECIFIED].includes(item.remoteLink?.linkState ?? LinkState.OK) &&
        this.config.instances.some((i) => i.id === item.remoteLink?.instanceId && i.push);
      // The fields the instance keeps local (local_fields): the person's, never pushed.
      const local = this.config.instances.find((i) => i.id === item.remoteLink?.instanceId)?.localFields ?? [];
      const tracker = changes.filter((c) => CORE_FIELDS.has(c.field) && !local.includes(CORE_PATHS[c.field]!));
      const core = item.remoteLink !== undefined ? tracker.find((c) => !(pushes && PUSHED_FIELDS.has(c.field))) : undefined;
      if (core) {
        const path = CORE_PATHS[core.field]!;
        throw errorInfo(
          Code.FailedPrecondition,
          "LINKED_READ_ONLY",
          inBatch(p, `${path} is the tracker's: the item is linked; change it there`),
          { field: path, item_id: p.id },
        );
      }
      // Someone else's issue (design 5.9): a pushed edit needs the say-so.
      const theirs = item.remoteLink !== undefined && !item.isMine && !p.confirmNotMine ? tracker[0] : undefined;
      if (theirs) {
        const path = CORE_PATHS[theirs.field]!;
        throw errorInfo(
          Code.FailedPrecondition,
          "NOT_MINE",
          inBatch(p, `${path} would change in the tracker an issue assigned to someone else: confirm to change it (confirm_not_mine)`),
          { field: path, item_id: p.id },
        );
      }
      // As the core maintains status_entered_at, mutation by mutation.
      if (next.status !== item.status) batch.statusChanged.add(p.id);
      const parent = changes.find((c) => c.field === "parentId")?.new;
      const problem = typeof parent === "string" ? batch.parentProblem(p.id, parent) : undefined;
      if (problem) throw badRequest(`${p.prefix}item.parent_id`, problem);
      const changed = changes.length > 0;
      if (p.kind === "delete" && changed) {
        // The live descendants go too, deepest first, then the Item: undo
        // works in reverse, so it undeletes top-down.
        const descendants = batch.liveDescendants(p.id);
        for (const id of [...descendants].reverse()) {
          const s = batch.load(id);
          const d = applyPatch(batch.items[s]!, { deletedAt: now });
          batch.record(s, d.next, d.changes);
        }
        for (const id of descendants) if (!named.has(id)) subtasks.add(id);
        batch.record(slot, next, changes);
      } else if (p.kind === "undelete" && changed) {
        batch.record(slot, next, changes);
        undeleted.push([p, p.id]);
        if (deletedBy !== undefined) {
          for (const id of batch.undeleteDescendants(p.id, deletedBy)) {
            undeleted.push([p, id]);
            if (!named.has(id)) subtasks.add(id);
          }
        }
      } else {
        batch.record(slot, next, changes);
      }
      slots.push(slot);
    }
    const orphan = batch.orphan(undeleted);
    if (orphan) {
      const [p, id, parentId] = orphan;
      const message = `item ${id} cannot be undeleted while its parent ${parentId} is deleted: undelete the parent first`;
      throw errorInfo(Code.FailedPrecondition, "PARENT_DELETED", inBatch(p, message), { item_id: id, parent_id: parentId });
    }
    const generated = opts.generate === false ? [] : this.generate(batch, slots, today, now);
    const actions = planned.map((p, i) => ({
      verb: verbOf(p),
      parens: p.kind === "setStatus" || p.kind === "setRecurrence",
      id: p.id,
      title: batch.items[slots[i]!]!.title,
    }));
    const dates = generated.map((s) => dayOf(batch.items[s]!.dueDate!));
    const summary = kind === OpKind.APPLY_SUGGESTION ? applySummary(actions) : mutationSummary(actions, subtasks.size, dates, today);
    const op = this.commit(batch, kind, summary, now);
    const result = this.record(rpc, requestId, planned[0]!.id, op);
    return {
      items: slots.map((s) => this.present(batch.items[s]!, today)),
      result,
      generated: generated.map((s) => this.present(batch.items[s]!, today)),
    };
  }

  /**
   * The recurrence post-pass (design 3.8), as generate_tx runs it: for each
   * Item the op completed, in mutation order, the next instance of its
   * series, created in the same op. Returns their slots.
   *
   * It runs on the op's final state, so `[x A, s A→next]`, `[x A, dd A]` and
   * `[x A, @clear A]` create nothing. It never creates an instance while
   * another instance of the series is live and open: the guard sees the
   * working copies, the instances this pass has made, and the stored Items.
   * An instance whose parent is gone or deleted starts at the top level.
   */
  private generate(batch: Working, slots: readonly number[], today: number, now: Timestamp): number[] {
    const out: number[] = [];
    for (const slot of new Set(slots)) {
      const completed = batch.completedInstance(slot);
      if (!completed || batch.seriesIsOpen(completed.recurrence!.seriesId, completed.id)) continue;
      const next = nextInstance(completed, this.newId(), today, now)!;
      // The fake keeps no staleness signals on its Items.
      delete next.staleness;
      if (next.parentId !== undefined && batch.unusableParent(next.parentId)) delete next.parentId;
      out.push(batch.create(next));
    }
    return out;
  }

  /**
   * Undo, or Redo when `redo`: reverts the top entry's events (for undo the
   * entry's latest redo, else the entry; for redo its latest undo) under the
   * guard, as a new op linked to the entry.
   */
  private undoTx(rpc: "Undo" | "Redo", req: UndoRequest | RedoRequest, redo: boolean) {
    this.check();
    const requestId = requestIdOf(req.requestId);
    const expected = req.expectedOpId === undefined ? undefined : canonicalUlid(req.expectedOpId);
    if (expected === null) throw badRequest("expected_op_id", "must be a ULID");
    const recorded = this.recorded(rpc, requestId);
    if (recorded) {
      const { op, result } = recorded;
      return {
        result,
        entry: this.summaryOf(this.ops.find((o) => o.id === op.entryOp)),
        items: itemIdsOf(op.events).map((id) => this.get(id)),
        skipped: [...(op.skipped ?? [])],
      };
    }
    const entry = this.top(redo);
    if (expected !== undefined && entry?.id !== expected) {
      const found = entry ? `the next is ${entry.id}` : "it is empty";
      throw errorInfo(Code.Aborted, "UNDO_STACK_CHANGED", `the undo stack changed: expected entry ${expected}, but ${found}`, {
        expected_op_id: expected,
        top_op_id: entry?.id ?? "",
      });
    }
    const verb = redo ? "redo" : "undo";
    const kind = redo ? OpKind.REDO : OpKind.UNDO;
    if (!entry) {
      // Recorded all the same, so a late retry cannot act on a newer entry.
      const op = this.log({ kind, actor: "user", summary: `nothing to ${verb}`, events: [] });
      return { result: this.record(rpc, requestId, "", op), items: [], skipped: [] };
    }
    const summary = `${verb}: ${entry.summary}`;
    const now = this.stamp();
    const source = this.sourceOf(entry, redo);
    const steps = inverse(source.events, now);
    // The parent rule holds where the undo leaves the Items, as for a batch,
    // not after each step: a batch may pass through a live Item under a
    // deleted parent, and its undo then passes through it too, in reverse.
    // A pass that leaves the rule broken is rolled back and run again
    // without the steps that broke it, which are reported.
    const ruledOut = new Map<number, string>();
    let pass = this.undoPass(steps, ruledOut);
    for (let broken = pass.broken(); broken.size > 0; broken = pass.broken()) {
      for (const [at, reason] of broken) ruledOut.set(at, reason);
      this.relations.clear();
      for (const [key, relation] of pass.relations) this.relations.set(key, relation);
      pass = this.undoPass(steps, ruledOut);
    }
    const { batch, skipped, changed } = pass;
    const summarized = this.summaryOf(entry);
    if (!changed) {
      // Nothing to revert: the entry leaves both stacks, no Item is written.
      entry.undoState = "dropped";
      const op = this.log({ kind, actor: "user", summary, events: [], entryOp: entry.id, skipped });
      return { result: this.record(rpc, requestId, "", op), entry: summarized, items: [], skipped };
    }
    const op = this.commit(batch, kind, summary, now, { entryOp: entry.id, skipped });
    this.noteEntered(op, this.enteredRestores(source, pass.statusRestored));
    entry.undoState = redo ? "done" : "undone";
    // As on_undo: each patch the apply applied whose field the undo restored
    // waits again (a status pair as one unit); a redo applies it again.
    const [from, to] = redo ? [PatchState.PENDING, PatchState.APPLIED] : [PatchState.APPLIED, PatchState.PENDING];
    const moved: Patch[] = [];
    for (const p of this.appliedBy.get(entry.id) ?? []) {
      const s = [...this.suggestions.values()].find((x) => x.patches.includes(p));
      const fields = s && pass.fieldsRestored.get(s.itemId);
      const field = localOf(p.field);
      const back = fields !== undefined && (fields.has(field) || (TRIPLE.includes(field) && [...fields].some((f) => TRIPLE.includes(f))));
      if (back && p.state === from) {
        p.state = to;
        moved.push(p);
      }
    }
    const suggestions = [...this.suggestions.values()].filter((s) => s.patches.some((p) => moved.includes(p)));
    for (const s of suggestions) {
      s.state = settledState(s);
      this.revision++;
      this.publish({ revision: this.revision, topic: Topic.SUGGESTION, id: s.id, resource: { case: "suggestion", value: s } });
      this.flag(s.itemId);
    }
    // The flags are the op's: its Items answer with them, at its last revision.
    const items = itemIdsOf(batch.events).map((id) => this.items.get(id) ?? batch.items[batch.slots.get(id)!]!);
    const result = this.record(rpc, requestId, "", op);
    return { result: { ...result, revision: this.revision }, entry: summarized, items, skipped };
  }

  /**
   * Runs an undo's `steps` on fresh working copies, except the ones
   * `ruledOut`, which are skipped with their reason. `relations` is the
   * relation set from before the pass, for a rerun; `broken` the steps
   * that leave a live Item under a deleted parent, with why each is ruled
   * out: of a step that undeleted or moved the Item and one that deleted
   * its parent, the later one is.
   */
  private undoPass(steps: readonly InverseStep[], ruledOut: ReadonlyMap<number, string>) {
    const relations = new Map(this.relations);
    const batch = this.batch();
    const skipped: UndoSkip[] = [];
    // Per Item, the last step that undeleted it or changed its parent, and
    // the last one that deleted it.
    const placed = new Map<string, number>();
    const deleted = new Map<string, number>();
    // The Items whose status a unit restored, for their status_entered_at.
    const statusRestored = new Set<string>();
    // The fields each Item had restored: an apply's patches wait again only for these.
    const fieldsRestored = new Map<string, Set<string>>();
    let changed = false;
    for (const [at, step] of steps.entries()) {
      if (step.kind === "restore") {
        const { itemId, unit } = step;
        const reason = ruledOut.get(at);
        const outcome =
          reason === undefined
            ? this.restoreUnit(batch, itemId, unit)
            : create(UndoSkipSchema, { itemId, field: skipField(unit.map((r) => r.field)), reason: UndoSkipReason.INVALID, message: reason });
        if (outcome !== "restored") {
          skipped.push(outcome);
          continue;
        }
        changed = true;
        if (unit.some((r) => TRIPLE.includes(r.field))) statusRestored.add(itemId);
        const fields = fieldsRestored.get(itemId) ?? new Set<string>();
        for (const r of unit) fields.add(r.field);
        fieldsRestored.set(itemId, fields);
        for (const r of unit) {
          if (r.field === "deletedAt" && r.value !== undefined) deleted.set(itemId, at);
          else if (r.field === "deletedAt" || r.field === "parentId") placed.set(itemId, at);
        }
        continue;
      }
      const outcome = this.changeRelation(batch, step.relation, step.kind === "addRelation");
      if (outcome === "changed") {
        changed = true;
        continue;
      }
      const [reason, message] =
        outcome === "unchanged"
          ? [UndoSkipReason.CHANGED_SINCE, "the relation has changed since"]
          : [UndoSkipReason.INVALID, `item ${outcome.missing} does not exist`];
      skipped.push(create(UndoSkipSchema, { itemId: step.relation.fromId, reason, message, relation: step.relation }));
    }
    const broken = () => {
      const out = new Map<number, string>();
      const blame = (child: string, parent: string) => {
        const [p, d] = [placed.get(child), deleted.get(parent)];
        if (p !== undefined && !(d !== undefined && d > p)) out.set(p, "the parent is deleted");
        else if (d !== undefined) out.set(d, "the Item has live subtasks");
      };
      for (const child of placed.keys()) {
        const item = batch.items[batch.slots.get(child)!]!;
        if (item.parentId !== undefined && item.deletedAt === undefined && batch.isDeleted(item.parentId)) blame(child, item.parentId);
      }
      for (const parent of deleted.keys()) {
        if (!batch.isDeleted(parent)) continue;
        for (const [child, gone] of batch.children(parent)) if (!gone) blame(child, parent);
      }
      return out;
    };
    return { batch, skipped, changed, relations, broken, statusRestored, fieldsRestored };
  }

  /**
   * Applies one unit of restores to `itemId`'s working copy, or says why
   * not: the Item is gone, deleted (only a deleted_at unit runs on a
   * tombstone), a field no longer holds the expected value, or the result
   * is not valid (an inconsistent status triple, a parent that is missing
   * or a descendant). Whether it keeps every live Item's parent live is
   * checked once the whole undo has run.
   */
  private restoreUnit(batch: Working, itemId: string, unit: readonly FieldRestore[]): "restored" | UndoSkip {
    const fields = unit.map((r) => r.field);
    const skip = (reason: UndoSkipReason, message = SKIP_MESSAGES[reason] ?? "") =>
      create(UndoSkipSchema, { itemId, field: skipField(fields), reason, message });
    const slot = batch.find(itemId);
    if (slot === undefined) return skip(UndoSkipReason.INVALID, "no such Item");
    const item = batch.items[slot]!;
    if (item.deletedAt !== undefined && !fields.includes("deletedAt")) return skip(UndoSkipReason.ITEM_DELETED);
    const next = { ...item } as unknown as Fields;
    const changes: FieldChange[] = [];
    for (const { field, expected, value } of unit) {
      const old = next[field];
      if (!matches(field, old, expected)) return skip(UndoSkipReason.CHANGED_SINCE);
      next[field] = value;
      if (!sameField(field, old, value)) changes.push({ field, old, new: value });
    }
    const after = next as unknown as Item;
    if (fields.some((f) => TRIPLE.includes(f)) && after.resolutionKind !== undefined && after.statusCategory !== StatusCategory.DONE) {
      return skip(UndoSkipReason.INVALID, "a resolution kind is only valid in the done category");
    }
    for (const c of changes) {
      const problem = c.field === "parentId" && typeof c.new === "string" ? batch.parentProblem(itemId, c.new, true) : undefined;
      if (problem) return skip(UndoSkipReason.INVALID, problem);
    }
    after.status = deriveStatus(after);
    batch.record(slot, after, changes);
    return "restored";
  }

  /**
   * The status_entered_at changes an undo (or redo) of `source` makes, as
   * the store restores the signal: back to what `source` found, for each
   * Item whose status the undo restored, where the signal still holds what
   * `source` wrote.
   */
  private enteredRestores(source: FakeOp, statusRestored: ReadonlySet<string>): Entered[] {
    const out: Entered[] = [];
    for (const e of [...(this.statusEnteredBy.get(source.id) ?? [])].reverse()) {
      const current = this.statusEnteredAt.get(e.id);
      if (statusRestored.has(e.id) && current === e.new) out.push({ id: e.id, old: current, new: e.old });
    }
    return out;
  }

  /** AddRelation (`add`) or RemoveRelation. */
  private relationTx(rpc: string, req: AddRelationRequest | RemoveRelationRequest, add: boolean) {
    this.check();
    const requestId = requestIdOf(req.requestId);
    const asked = relationOf(req.relation);
    if (asked.fromId === asked.toId) {
      // An Item related to itself has a wrong to_id; when its from_id is
      // wrong too, that is the end named, as when both ends are.
      const at = this.batch().parentOf(asked.fromId);
      if (at === undefined) throw relationEnd(asked, asked.fromId, "missing");
      if (add && at.deleted) throw relationEnd(asked, asked.fromId, "deleted");
      throw badRequest("relation.to_id", "must differ from from_id");
    }
    const relation = canonical(asked);
    // The same request names the same relation, whole.
    const recorded = this.recorded(rpc, requestId, relationKey(relation));
    if (recorded) return { fromItem: this.get(relation.fromId), toItem: this.get(relation.toId), result: recorded.result };
    const batch = this.batch();
    // The request's from_id first, whichever way round a RELATES_TO is stored.
    for (const end of [asked.fromId, asked.toId]) {
      const at = batch.parentOf(end);
      if (at === undefined) throw relationEnd(asked, end, "missing");
      if (add && at.deleted) throw relationEnd(asked, end, "deleted");
    }
    const from = batch.load(relation.fromId);
    const to = batch.load(relation.toId);
    this.changeRelation(batch, relation, add);
    const words = RELATION_WORDS[relation.kind];
    const summary = `${add ? "relate" : "unrelate"} ${quoted(batch.items[from]!.title)} ${words} ${quoted(batch.items[to]!.title)}`;
    const op = this.commit(batch, add ? OpKind.ADD_RELATION : OpKind.REMOVE_RELATION, summary, this.stamp());
    return { fromItem: batch.items[from]!, toItem: batch.items[to]!, result: this.record(rpc, requestId, relationKey(relation), op) };
  }

  /**
   * Adds `relation` if it is absent (`add`), or removes it if it is present,
   * as an event of the op `batch` builds that touches both ends, deleted or
   * not. A missing end changes nothing.
   */
  private changeRelation(batch: Working, relation: Relation, add: boolean): "changed" | "unchanged" | { missing: string } {
    for (const end of [relation.fromId, relation.toId]) if (batch.parentOf(end) === undefined) return { missing: end };
    const key = relationKey(relation);
    if (this.relations.has(key) === add) return "unchanged";
    if (add) this.relations.set(key, clone(RelationSchema, relation));
    else this.relations.delete(key);
    for (const end of [relation.fromId, relation.toId]) {
      const slot = batch.load(end);
      batch.touch(slot, { ...batch.items[slot]!, relations: this.relationsOf(end) });
    }
    batch.events.push({ kind: add ? "relationAdd" : "relationRemove", itemId: relation.fromId, relation: clone(RelationSchema, relation) });
    return "changed";
  }

  /**
   * Writes the op `batch` built: one revision (in first-touch order),
   * `updated_at` and Change per touched Item, then the op itself.
   */
  private commit(
    batch: Working,
    kind: OpKind,
    summary: string,
    now: Timestamp,
    { actor = "user", ...extra }: Pick<FakeOp, "entryOp" | "skipped"> & { actor?: FakeActor } = {},
  ): FakeOp {
    // The fan-out (relatives whose counts or parent title change) is
    // touched without updated_at, as Batch::fan_out does it.
    const loaded = batch.items.map((item) => this.items.get(item.id));
    for (const id of this.relatives(loaded, batch.items)) {
      const slot = batch.find(id);
      if (slot !== undefined && !batch.isTouched(slot)) batch.touch(slot, { ...batch.items[slot]! }, { fanout: true });
    }
    const touched = batch.touched();
    for (const slot of touched) {
      const item = batch.items[slot]!;
      if (!batch.isFanout(slot)) item.updatedAt = now;
      item.revision = ++this.revision;
      this.items.set(item.id, this.settle(item));
    }
    // Once every touched Item is stored, so the counts are the op's.
    for (const slot of touched) {
      const item = this.hierarchy(batch.items[slot]!);
      this.publish({ revision: item.revision, topic: Topic.ITEM, id: item.id, resource: { case: "item", value: item } });
    }
    const op = this.log({ kind, actor, summary, events: batch.events, ...extra });
    // A mutation that changed an Item's status starts its clock; so does a
    // creation, whose twin has the same signals and so records nothing.
    const at = nanosOf(now);
    this.noteEntered(op, [...batch.statusChanged].map((id) => ({ id, old: this.statusEnteredAt.get(id), new: at })));
    for (const slot of batch.created) this.statusEnteredAt.set(batch.items[slot]!.id, at);
    return op;
  }

  /** Applies what `op` did to the status_entered_at of Items, and keeps it for undo and redo. */
  private noteEntered(op: FakeOp, entered: Entered[]): void {
    for (const e of entered) {
      if (e.new === undefined) this.statusEnteredAt.delete(e.id);
      else this.statusEnteredAt.set(e.id, e.new);
    }
    if (entered.length > 0) this.statusEnteredBy.set(op.id, entered);
  }

  /**
   * The scheduler's sweep (design 3.7), as auto_archive_tx runs it, now: ops
   * by the system (never undo stack entries) of kind AUTO_ARCHIVE, each of at
   * most 1000 Items, until nothing more is due.
   *
   * - **Healing first:** live Items archived AUTO_DONE that are open again
   *   are unarchived, in archive order.
   * - **The auto_done archive:** live, unarchived done or cancelled Items
   *   that closed at least `after` milliseconds ago (default 7 days) are
   *   archived AUTO_DONE, oldest close first. An Item closes when it enters
   *   done or cancelled (done → cancelled closes it again), as the store's
   *   status_entered_at says; an Item put without one falls back to its
   *   updated_at. An Item whose archive the user set (a `z`, either way) at
   *   or after its close is skipped, until it closes again. Only a decision
   *   still in effect counts: not one of the system, of an undo or redo, or
   *   of an op that is undone or discarded.
   */
  runScheduler(after: number = (this.config.ui?.archiveDoneAfterDays ?? 7) * DAY_MS): FakeSweep {
    const out: FakeSweep = { archived: [], healed: [], ops: [] };
    for (let more = true; more; ) {
      const now = this.stamp();
      let healed = this.reopened();
      more = healed.length > AUTO_ARCHIVE_LIMIT;
      healed = healed.slice(0, AUTO_ARCHIVE_LIMIT);
      const room = AUTO_ARCHIVE_LIMIT - healed.length;
      let archived = more ? [] : this.closedBefore(nanosOf(now) - BigInt(Math.trunc(after)) * 1_000_000n);
      more ||= archived.length > room;
      archived = archived.slice(0, room);
      if (healed.length === 0 && archived.length === 0) break;
      const batch = this.batch();
      const sweeps: [string[], () => Fields][] = [
        [healed, () => ({ archived: undefined })],
        [archived, () => ({ archived: create(ArchivedSchema, { at: clone(TimestampSchema, now), reason: ArchiveReason.AUTO_DONE }) })],
      ];
      for (const [ids, patch] of sweeps) {
        for (const id of ids) {
          const slot = batch.load(id);
          const { next, changes } = applyPatch(batch.items[slot]!, patch());
          batch.record(slot, next, changes);
        }
      }
      out.ops.push(this.commit(batch, OpKind.AUTO_ARCHIVE, autoArchiveSummary(archived.length, healed.length), now, { actor: "system" }));
      out.archived.push(...archived);
      out.healed.push(...healed);
    }
    return out;
  }

  /** The live Items archived AUTO_DONE that are open again, in archive order. */
  private reopened(): string[] {
    return [...this.items.values()]
      .filter((i) => i.archived?.reason === ArchiveReason.AUTO_DONE && !isClosed(i) && i.deletedAt === undefined)
      .map((i) => ({ id: i.id, at: nanosOf(i.archived!.at) }))
      .sort((a, b) => compareBig(a.at, b.at))
      .map((r) => r.id);
  }

  /** The live, unarchived done or cancelled Items that closed at or before `cutoff` (nanoseconds) and that no user archive keeps, oldest close first. */
  private closedBefore(cutoff: bigint): string[] {
    return [...this.items.values()]
      .filter((i) => isClosed(i) && i.archived === undefined && i.deletedAt === undefined)
      .map((i) => ({ id: i.id, closed: this.statusEnteredAt.get(i.id) ?? nanosOf(i.updatedAt) }))
      .filter((r) => r.closed <= cutoff && !this.archiveDecided(r.id, r.closed))
      .sort((a, b) => compareBig(a.closed, b.closed))
      .map((r) => r.id);
  }

  /** Whether an op still in effect, by the user, set `id`'s archive at or after `since` (nanoseconds). */
  private archiveDecided(id: string, since: bigint): boolean {
    return this.ops.some(
      (op) =>
        op.actor !== "system" &&
        op.kind !== OpKind.UNDO &&
        op.kind !== OpKind.REDO &&
        op.undoState !== "undone" &&
        op.undoState !== "discarded" &&
        nanosOf(op.at) >= since &&
        op.events.some((e) => e.kind === "set" && e.itemId === id && e.field === "archived"),
    );
  }

  /**
   * Appends an op at the current revision. An op by the user that recorded
   * an event, other than an Undo or Redo, becomes the top undo stack entry
   * and discards the redo stack.
   */
  private log(parts: Pick<FakeOp, "kind" | "actor" | "summary" | "events" | "entryOp" | "skipped">): FakeOp {
    const entry = parts.actor === "user" && parts.kind !== OpKind.UNDO && parts.kind !== OpKind.REDO && parts.events.length > 0;
    if (entry) for (const op of this.ops) if (op.undoState === "undone") op.undoState = "discarded";
    const op: FakeOp = { id: ulid(), seq: this.ops.length + 1, at: this.stamp(), revision: this.revision, undoState: entry ? "done" : undefined, ...parts };
    this.ops.push(op);
    return op;
  }

  /** The entry Undo (or Redo, when `redo`) would act on. */
  private top(redo: boolean): FakeOp | undefined {
    return redo ? this.ops.find((o) => o.undoState === "undone") : this.ops.findLast((o) => o.undoState === "done");
  }

  /** The op whose events an undo (or redo) of `entry` inverts. */
  private sourceOf(entry: FakeOp, redo: boolean): FakeOp {
    const kind = redo ? OpKind.UNDO : OpKind.REDO;
    const latest = this.ops.findLast((o) => o.entryOp === entry.id && o.kind === kind);
    if (latest) return latest;
    if (!redo) return entry;
    throw new ConnectError(`undone entry ${entry.id} has no undo op`, Code.Internal);
  }

  private summaryOf(op: FakeOp | undefined): OpSummary | undefined {
    return op && create(OpSummarySchema, { opId: op.id, kind: op.kind, itemIds: itemIdsOf(op.events), at: op.at, description: op.summary });
  }

  /** Every relation with `id` at either end, sorted by (from_id, to_id, kind). */
  private relationsOf(id: string): Relation[] {
    return [...this.relations.values()]
      .filter((r) => r.fromId === id || r.toId === id)
      .sort((a, b) => (a.fromId !== b.fromId ? cmp(a.fromId, b.fromId) : a.toId !== b.toId ? cmp(a.toId, b.toId) : a.kind - b.kind))
      .map((r) => clone(RelationSchema, r));
  }

  private batch(): Working {
    return new Working(this.items, this.ops);
  }

  /**
   * The request recorded under `requestId`, if any. Fails with
   * INVALID_ARGUMENT when it was another RPC, or a single-item mutation of
   * an Item other than `id`.
   */
  private recorded(rpc: string, requestId: string, id?: string): Recorded | undefined {
    const recorded = this.requests.get(requestId);
    if (recorded && (recorded.rpc !== rpc || (id !== undefined && recorded.id !== id))) {
      throw badRequest("request_id", "was already used for a different request");
    }
    return recorded;
  }

  /** Remembers `op`'s result under `requestId`, for replays. */
  private record(rpc: string, requestId: string, id: string, op: FakeOp): MutationResult {
    const result = { opId: op.id, revision: op.revision };
    this.requests.set(requestId, { rpc, id, result, op });
    return result;
  }

  private get(id: string): Item {
    const item = this.items.get(id);
    if (!item) throw notFound(id);
    return item;
  }

  private newId(): string {
    return `01FAKE${String(++this.nextId).padStart(20, "0")}`;
  }

  private publish(init: MessageInitShape<typeof ChangeSchema>): void {
    const change = create(ChangeSchema, init);
    this.changes.push(change);
    for (const w of [...this.watchers]) w(msg({ case: "change", value: this.asSent(change) }));
  }

  /** A change as sent: config is not stored, so a config change carries the config current now. */
  private asSent(change: Change): Change {
    if (change.topic !== Topic.CONFIG || change.deleted) return change;
    return create(ChangeSchema, { ...change, resource: { case: "config", value: this.config } });
  }

  private check(): void {
    if (this.down) throw new ConnectError("daemon down", Code.Unavailable);
  }
}

/** Marks the op being built in {@link Working.deletedBy}. */
const THIS_OP = Symbol("this op");

/**
 * The working copies of the Items an op has loaded, in first-touch order,
 * with the events it has made so far; the stored Items are untouched until
 * the op commits.
 */
class Working {
  readonly items: Item[] = [];
  readonly slots = new Map<string, number>();
  readonly events: FakeEvent[] = [];
  /** Slots that get a revision: the ones with events, relation ends, and the fan-out. */
  private readonly dirty = new Set<number>();
  /** Slots re-emitted only for their hierarchy fields: no updated_at, not part of the op's answer. */
  private readonly fanout = new Set<number>();
  /** Items this op soft-deleted and has not undeleted since. */
  private readonly deletedHere = new Set<string>();
  /** Slots this op created ({@link create}), in creation order. */
  readonly created: number[] = [];
  /** Items a mutation of this op changed the derived status of, so their status_entered_at is the op's. */
  readonly statusChanged = new Set<string>();

  constructor(
    private readonly stored: ReadonlyMap<string, Item>,
    private readonly ops: readonly FakeOp[],
  ) {}

  /** The slot of `id`'s working copy, loading it on first use; undefined when no such Item exists. */
  find(id: string): number | undefined {
    const slot = this.slots.get(id);
    if (slot !== undefined) return slot;
    const item = this.stored.get(id);
    if (!item) return undefined;
    this.slots.set(id, this.items.push(item) - 1);
    return this.items.length - 1;
  }

  /** As find, failing with NOT_FOUND. */
  load(id: string): number {
    const slot = this.find(id);
    if (slot === undefined) throw notFound(id);
    return slot;
  }

  /** Takes `next` as the slot's Item, with an event per change. */
  record(slot: number, next: Item, changes: readonly FieldChange[]): void {
    for (const c of changes) {
      if (c.field !== "deletedAt") continue;
      if (c.new === undefined) this.deletedHere.delete(next.id);
      else this.deletedHere.add(next.id);
    }
    if (changes.length > 0) this.dirty.add(slot);
    for (const c of changes) this.events.push({ kind: "set", itemId: next.id, field: c.field, old: c.old, new: c.new });
    this.items[slot] = next;
  }

  /**
   * Adds `item` as a new Item of this op, with a `create` event, and returns
   * its slot. It has no stored twin, so it contributes nothing to its
   * parent's counts as loaded and the commit's fan-out re-emits the parent.
   */
  create(item: Item): number {
    const slot = this.items.push(item) - 1;
    this.slots.set(item.id, slot);
    this.events.push({ kind: "create", itemId: item.id });
    this.created.push(slot);
    this.dirty.add(slot);
    return slot;
  }

  /**
   * The Item in `slot` when this op completed it and it still repeats: it
   * was open before the op, is done or cancelled now (so done → cancelled is
   * no completion), is live, and kept a rule. It is the template of the next
   * instance (design 3.8).
   */
  completedInstance(slot: number): Item | undefined {
    const item = this.items[slot]!;
    const loaded = this.stored.get(item.id);
    const closedHere = loaded !== undefined && !isClosed(loaded) && isClosed(item);
    return closedHere && item.deletedAt === undefined && item.recurrence?.rule !== undefined ? item : undefined;
  }

  /**
   * Whether an instance of `seriesId` other than `but` is still live and
   * open, as of this point in the op: a working copy (which includes the
   * instances this op has created), or a stored Item the op has not loaded.
   */
  seriesIsOpen(seriesId: string, but: string): boolean {
    const open = (item: Item) => item.id !== but && item.deletedAt === undefined && !isClosed(item) && item.recurrence?.seriesId === seriesId;
    if (this.items.some(open)) return true;
    for (const item of this.stored.values()) if (!this.slots.has(item.id) && open(item)) return true;
    return false;
  }

  /** Takes `next` as the slot's Item without an event of its own (a relation end, or with `fanout` a relative). */
  touch(slot: number, next: Item, opts: { fanout?: boolean } = {}): void {
    this.items[slot] = next;
    this.dirty.add(slot);
    if (opts.fanout) this.fanout.add(slot);
  }

  isTouched(slot: number): boolean {
    return this.dirty.has(slot);
  }

  isFanout(slot: number): boolean {
    return this.fanout.has(slot);
  }

  /** The touched slots, in first-touch order. */
  touched(): number[] {
    return [...this.dirty].sort((a, b) => a - b);
  }

  /** `id`'s parent and whether it is deleted, as of this point in the op; undefined when no such Item exists. */
  parentOf(id: string): { parentId: string | undefined; deleted: boolean } | undefined {
    const slot = this.slots.get(id);
    const item = slot === undefined ? this.stored.get(id) : this.items[slot];
    return item && { parentId: item.parentId, deleted: item.deletedAt !== undefined };
  }

  isDeleted(id: string): boolean {
    return this.parentOf(id)?.deleted ?? false;
  }

  /**
   * `id`'s children and whether each is deleted, as of this point in the op:
   * stored children the op has not moved away (by created_at, then id), then
   * working copies it moved under `id`.
   */
  children(id: string): [string, boolean][] {
    const out: [string, boolean][] = [];
    for (const child of [...this.stored.values()].filter((i) => i.parentId === id).sort(byCreation)) {
      const slot = this.slots.get(child.id);
      const item = slot === undefined ? child : this.items[slot]!;
      if (item.parentId === id) out.push([item.id, item.deletedAt !== undefined]);
    }
    for (const item of this.items) {
      if (item.parentId === id && !out.some(([c]) => c === item.id)) out.push([item.id, item.deletedAt !== undefined]);
    }
    return out;
  }

  /** `id`'s live descendants, top-down (breadth first). */
  liveDescendants(id: string): string[] {
    const seen = new Set([id]);
    const queue = [id];
    const out: string[] = [];
    for (let parent = queue.shift(); parent !== undefined; parent = queue.shift()) {
      for (const [child, deleted] of this.children(parent)) {
        if (deleted) continue;
        if (seen.has(child)) throw new ConnectError(`the descendants of ${id} loop at ${child}`, Code.Internal);
        seen.add(child);
        out.push(child);
        queue.push(child);
      }
    }
    return out;
  }

  /** The op of `id`'s latest deleted_at event (THIS_OP for this one); undefined for an Item never deleted through an op. */
  deletedBy(id: string): string | typeof THIS_OP | undefined {
    if (this.deletedHere.has(id)) return THIS_OP;
    const isDeletedAt = (e: FakeEvent) => e.kind === "set" && e.itemId === id && e.field === "deletedAt";
    return this.ops.findLast((op) => op.events.some(isDeletedAt))?.id;
  }

  /**
   * Undeletes, top-down, the descendants of the just undeleted `id` that
   * `by` deleted, stopping at the ones it did not. Returns them.
   */
  undeleteDescendants(id: string, by: string | typeof THIS_OP): string[] {
    const seen = new Set([id]);
    const queue = [id];
    const restored: string[] = [];
    for (let parent = queue.shift(); parent !== undefined; parent = queue.shift()) {
      for (const [child, deleted] of this.children(parent)) {
        if (!deleted || this.deletedBy(child) !== by) continue;
        if (seen.has(child)) throw new ConnectError(`the descendants of ${id} loop at ${child}`, Code.Internal);
        seen.add(child);
        const slot = this.load(child);
        const { next, changes } = applyPatch(this.items[slot]!, { deletedAt: undefined });
        this.record(slot, next, changes);
        restored.push(child);
        queue.push(child);
      }
    }
    return restored;
  }

  /**
   * The parent rule for the Items an op undeleted, with the mutation that
   * did, in op order, as the op leaves them: the first still live under a
   * deleted parent, as `[mutation, Item, parent]` for the last mutation that
   * undeleted it.
   */
  orphan<M>(undeleted: readonly (readonly [M, string])[]): [M, string, string] | undefined {
    const last = new Map(undeleted.map(([, id], at) => [id, at]));
    for (const [at, [m, id]] of undeleted.entries()) {
      if (last.get(id) !== at) continue;
      const item = this.items[this.slots.get(id)!]!;
      if (item.parentId !== undefined && item.deletedAt === undefined && this.isDeleted(item.parentId)) return [m, id, item.parentId];
    }
    return undefined;
  }

  /** Why `parent` cannot be any Item's parent (missing or deleted), if it cannot. */
  unusableParent(parent: string): string | undefined {
    const at = this.parentOf(parent);
    if (at === undefined) return "no such Item";
    return at.deleted ? "the parent is deleted" : undefined;
  }

  /** Why `parent` cannot become `id`'s parent, if it cannot; a deleted `parent` can when `deletedOk`. */
  parentProblem(id: string, parent: string, deletedOk = false): string | undefined {
    const unusable = this.unusableParent(parent);
    if (unusable && !(deletedOk && unusable === "the parent is deleted")) return unusable;
    const seen = new Set<string>();
    for (let ancestor: string | undefined = parent; ancestor !== undefined; ancestor = this.parentOf(ancestor)?.parentId) {
      if (ancestor === id) return "would create a cycle";
      if (seen.has(ancestor)) throw new ConnectError(`the parent chain of ${parent} loops at ${ancestor}`, Code.Internal);
      seen.add(ancestor);
    }
    return undefined;
  }
}

/**
 * The fields a mutation sets on `item` (empty when it changes nothing). For
 * UpdateItem it checks the mask's paths in order, as the store applies them:
 * a path that repeats or overlaps an earlier one, a status, archived or
 * read-only path, a title that is empty once trimmed, too long or more than
 * one line, a parent_id that names the Item itself, a waiting's `on`, a
 * nested waiting path while waiting is unset, and an empty label or one
 * with whitespace or a control character; labels are deduplicated, keeping
 * the first.
 */
function patchOf(p: Planned, item: Item, now: Timestamp): Fields {
  switch (p.kind) {
    case "update": {
      const { paths, values } = p.mask!;
      const patch: Fields = {};
      for (const [at, path] of paths.entries()) {
        const top = path.split(".")[0]!;
        const overlaps = (earlier: string) => earlier === path || earlier === top || earlier.split(".")[0] === path;
        if (paths.slice(0, at).some(overlaps)) throw badRequest(`${p.prefix}update_mask`, `${JSON.stringify(path)} repeats or overlaps an earlier path`);
        const where = `${p.prefix}item.${path}`;
        if (STATUS_PATHS.has(path)) throw badRequest(where, "changes only through SetStatus");
        if (top === "archived") throw badRequest(where, "changes only through ArchiveItem and UnarchiveItem");
        if (!WRITABLE.has(top)) throw badRequest(where, "is read-only");
        const field = FIELDS_BY_NAME.get(top)!.localName;
        let value = values[path];
        if (path.startsWith("waiting.")) {
          // Only that field changes; the event still records the whole waiting.
          const waiting = (patch.waiting ?? item.waiting) as Waiting | undefined;
          if (waiting === undefined) throw badRequest(where, "cannot be set while waiting is unset");
          value = withWaitingField(waiting, path, value, p.prefix);
        } else if (field === "waiting" && value !== undefined) {
          const waiting = clone(WaitingSchema, value as Waiting);
          waiting.on = waitingOn(waiting.on!, p.prefix);
          value = waiting;
        }
        if (field === "title" && typeof value === "string") {
          value = rustTrim(value);
          if (value === "") throw badRequest(where, "must not be empty");
          if (overCaptureLimit(value as string)) throw badRequest(where, TOO_LONG);
          if (LINE_BREAK.test(value as string)) throw badRequest(where, "must be a single line");
        }
        // As the daemon reads them: an unset energy or priority is NONE,
        // and a number that names no value is refused.
        if ((field === "energy" || field === "priority") && !value) value = field === "energy" ? Energy.NONE : Priority.NONE;
        const known = field === "energy" ? Energy : field === "priority" || field === "localPriorityOverride" ? Priority : undefined;
        if (known && typeof value === "number" && (value === 0 || !(value in known))) throw badRequest(where, value === 0 ? "is unspecified" : `unknown enum value ${value}`);
        if (field === "parentId" && value === item.id) throw badRequest(where, "an Item cannot be its own parent");
        // Priority's domain has a "none": unset clears it to that, as the daemon does.
        if (field === "priority" && (value === undefined || value === Priority.UNSPECIFIED)) value = Priority.NONE;
        if (field === "labels" && Array.isArray(value)) {
          if (value.some((l: string) => l === "" || BAD_LABEL.test(l))) {
            throw badRequest(where, "labels must be non-empty and contain no whitespace or control characters");
          }
          value = [...new Set(value)];
        }
        patch[field] = value;
      }
      return patch;
    }
    case "setStatus": {
      const patch: Fields = p.status === undefined || deriveStatus(item) === p.status ? {} : { ...withStatus(item, p.status) };
      // Reopened, an Item leaves an auto_done archive in the same op (design 3.7); a user archive stays.
      if (p.status !== undefined && !CLOSED.has(p.status) && item.archived?.reason === ArchiveReason.AUTO_DONE) patch.archived = undefined;
      return patch;
    }
    case "setRecurrence":
      return recurrencePatch(p, item);
    case "archive":
      return { archived: item.archived ?? create(ArchivedSchema, { at: now, reason: ArchiveReason.USER }) };
    case "unarchive":
      return { archived: undefined };
    case "delete":
      return { deletedAt: item.deletedAt ?? now };
    case "undelete":
      return { deletedAt: undefined };
  }
}

/**
 * What SetRecurrence sets (design 3.8). No rule clears the recurrence. A
 * rule on an Item without recurrence starts the planned series, and on one
 * that recurs keeps its series. A fixed rule's occurrence_date is its first
 * date on or after the due date (else the planned today), which becomes the
 * due date if there is none; an after-completion rule's is the due date,
 * which is kept. A rule on a done or cancelled Item fails with ITEM_CLOSED.
 */
function recurrencePatch(p: Planned, item: Item): Fields {
  const { rule, seriesId, today } = p.recurrence!;
  if (rule === undefined) return { recurrence: undefined };
  if (isClosed(item)) {
    throw errorInfo(Code.FailedPrecondition, "ITEM_CLOSED", inBatch(p, `item ${p.id} is done or cancelled: reopen it first`), { item_id: p.id });
  }
  const due = item.dueDate === undefined ? undefined : dayOf(item.dueDate);
  const occurrence = isFixed(rule) ? firstOnOrAfter(rule, due ?? today) : due;
  const recurrence = create(ItemRecurrenceSchema, {
    seriesId: item.recurrence?.seriesId ?? seriesId,
    rule: ruleToProto(rule),
    ...(occurrence !== undefined && { occurrenceDate: protoDate(occurrence) }),
  });
  const dueDate = item.dueDate ?? (isFixed(rule) && occurrence !== undefined ? create(DateSchema, protoDate(occurrence)) : undefined);
  return { recurrence, dueDate };
}

/** `item` with `patch` applied and its changes, in patch order; `item` itself when nothing changes. */
function applyPatch(item: Item, patch: Fields): { next: Item; changes: FieldChange[] } {
  const before = item as unknown as Fields;
  const changes: FieldChange[] = [];
  for (const [field, value] of Object.entries(patch)) {
    if (!sameField(field, before[field], value)) changes.push({ field, old: before[field], new: value });
  }
  if (changes.length === 0) return { next: item, changes };
  const next = { ...before };
  for (const c of changes) next[c.field] = c.new;
  const out = next as unknown as Item;
  out.status = deriveStatus(out);
  return { next: out, changes };
}

/**
 * The inverse of `events` (in op order), in reverse event order: a `set`
 * restores `old` where the field still holds `new`, a run of `set` events of
 * one Item on the status triple (in triple order) as one unit; a `create`
 * soft-deletes at `now`; a relation add is removed and a removed relation
 * added back.
 */
function inverse(events: readonly FakeEvent[], now: Timestamp): InverseStep[] {
  const runs: FakeEvent[][] = [];
  for (const e of events) {
    const run = runs.at(-1);
    if (run && inOneStatusRun(run.at(-1)!, e)) run.push(e);
    else runs.push([e]);
  }
  return runs.reverse().map((run): InverseStep => {
    const first = run[0]!;
    switch (first.kind) {
      case "set":
        return {
          kind: "restore",
          itemId: first.itemId,
          unit: run.flatMap((e) => (e.kind === "set" ? [{ field: e.field, expected: e.new, value: e.old }] : [])),
        };
      case "create":
        return { kind: "restore", itemId: first.itemId, unit: [{ field: "deletedAt", expected: undefined, value: now }] };
      case "relationAdd":
        return { kind: "removeRelation", relation: first.relation };
      case "relationRemove":
        return { kind: "addRelation", relation: first.relation };
    }
  });
}

/** Whether `b` continues `a`'s status run. */
function inOneStatusRun(a: FakeEvent, b: FakeEvent): boolean {
  if (a.kind !== "set" || b.kind !== "set" || a.itemId !== b.itemId) return false;
  const [at, bt] = [TRIPLE.indexOf(a.field), TRIPLE.indexOf(b.field)];
  return at >= 0 && bt > at;
}

/** The Items `events` name, in order; a relation event names both ends. */
function itemIdsOf(events: readonly FakeEvent[]): string[] {
  const ids: string[] = [];
  const name = (id: string) => {
    if (!ids.includes(id)) ids.push(id);
  };
  for (const e of events) {
    name(e.itemId);
    if (e.kind === "relationAdd" || e.kind === "relationRemove") name(e.relation.toId);
  }
  return ids;
}

/** The set events of what put changed. */
function diff(prev: Item, next: Item): FakeEvent[] {
  const [before, after] = [prev as unknown as Fields, next as unknown as Fields];
  return ItemSchema.fields
    .filter((f) => !UNTRACKED.has(f.localName) && !sameField(f.localName, before[f.localName], after[f.localName]))
    .map((f) => ({ kind: "set", itemId: next.id, field: f.localName, old: before[f.localName], new: after[f.localName] }));
}

/** Whether two values of Item field `field` are equal. */
function sameField(field: string, a: unknown, b: unknown): boolean {
  if (a === b) return true;
  const [x, y] = [create(ItemSchema) as unknown as Fields, create(ItemSchema) as unknown as Fields];
  x[field] = a;
  y[field] = b;
  return equals(ItemSchema, x as unknown as Item, y as unknown as Item);
}

/** The undo guard: equality, except that string lists (labels, ...) compare as sets. */
function matches(field: string, current: unknown, expected: unknown): boolean {
  const desc = FIELDS.get(field);
  if (desc?.fieldKind === "list" && desc.listKind === "scalar" && Array.isArray(current) && Array.isArray(expected)) {
    const [a, b] = [new Set(current), new Set(expected)];
    return a.size === b.size && [...a].every((v) => b.has(v));
  }
  return sameField(field, current, expected);
}

/** UndoSkip.field for a unit of `fields`: the status triple is ITEM_FIELD_STATUS. */
function skipField(fields: readonly string[]): ItemField {
  if (fields.some((f) => TRIPLE.includes(f))) return ItemField.STATUS;
  return (FIELDS.get(fields[0] ?? "")?.number ?? ItemField.UNSPECIFIED) as ItemField;
}

/** UndoSkip.message for a field unit, as the daemon words it; an INVALID one gives its own reason. */
const SKIP_MESSAGES: Partial<Record<UndoSkipReason, string>> = {
  [UndoSkipReason.CHANGED_SINCE]: "the field has changed since",
  [UndoSkipReason.ITEM_DELETED]: "the Item is deleted",
};

/** The SetStatus decomposition: todo statuses set the category and the refinement, the others keep the refinement. */
function withStatus(t: Triple, target: Status): Triple {
  const keep = t.todoRefinement || TodoRefinement.INBOX;
  switch (target) {
    case Status.INBOX:
      return INBOX_TRIPLE;
    case Status.NEXT:
      return { ...INBOX_TRIPLE, todoRefinement: TodoRefinement.NEXT };
    case Status.SOMEDAY:
      return { ...INBOX_TRIPLE, todoRefinement: TodoRefinement.SOMEDAY };
    case Status.IN_PROGRESS:
      return { statusCategory: StatusCategory.IN_PROGRESS, resolutionKind: undefined, todoRefinement: keep };
    case Status.DONE:
      return { statusCategory: StatusCategory.DONE, resolutionKind: ResolutionKind.DONE, todoRefinement: keep };
    case Status.CANCELLED:
      return { statusCategory: StatusCategory.DONE, resolutionKind: ResolutionKind.CANCELLED, todoRefinement: keep };
    default:
      return { statusCategory: t.statusCategory, resolutionKind: t.resolutionKind, todoRefinement: t.todoRefinement };
  }
}

/** The derived 6-value status of a triple (design 3.4). */
function deriveStatus(t: Triple): Status {
  switch (t.statusCategory) {
    case StatusCategory.IN_PROGRESS:
      return Status.IN_PROGRESS;
    case StatusCategory.DONE:
      return t.resolutionKind === ResolutionKind.CANCELLED ? Status.CANCELLED : Status.DONE;
    default:
      return t.todoRefinement === TodoRefinement.NEXT ? Status.NEXT : t.todoRefinement === TodoRefinement.SOMEDAY ? Status.SOMEDAY : Status.INBOX;
  }
}

/**
 * Keeps put's status fields consistent: a status given without the triple
 * (or a new Item's default) is decomposed onto the current triple; otherwise
 * the status is derived from the triple.
 */
function settleStatus(item: Item, keys: readonly string[], prev: Item | undefined): void {
  if (!keys.some((k) => TRIPLE.includes(k)) && (keys.includes("status") || !prev)) {
    Object.assign(item, withStatus(prev ?? INBOX_TRIPLE, item.status));
  }
  item.status = deriveStatus(item);
}

/** A summary verb, as summary.rs words it. */
function verbOf(p: Planned): string {
  switch (p.kind) {
    case "update":
      return "edit";
    case "setStatus":
      return STATUS_VERBS[p.status ?? Status.UNSPECIFIED] ?? "set status";
    case "setRecurrence":
      return p.recurrence?.rule === undefined ? "stop repeating" : "set repeat";
    default:
      return p.kind;
  }
}

const MONTH_NAMES = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAY_NAMES = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/** `Mon Nov 2`, with the year when it is not `today`'s (summary.rs `day`). */
function summaryDay(n: number, today: number): string {
  const { year, month, day } = civil(n);
  const text = `${WEEKDAY_NAMES[weekday(n) - 1]} ${MONTH_NAMES[month - 1]} ${day}`;
  return year === civil(today).year ? text : `${text} ${year}`;
}

/** `"<title>"`, cut to MAX_TITLE_CHARS characters with `…`. */
function quoted(title: string): string {
  const chars = [...title];
  return chars.length <= MAX_TITLE_CHARS ? `"${title}"` : `"${chars.slice(0, MAX_TITLE_CHARS - 1).join("")}…"`;
}

/**
 * The summary of a mutate op from its mutations in request order (titles as
 * after the op), the number of descendants a cascade took along that no
 * mutation names, and the due dates of the instances it generated, with
 * `today` deciding when a date needs its year: `mark done "x"`, `mark done
 * (3 items)` (a status or repeat verb, `parens`), `edit 3 items`, `change 3
 * items`, `delete "x" and 2 subtasks`, then `· next Mon Nov 2` for one Item
 * or `· 2 next` for several.
 */
function mutationSummary(
  actions: readonly { verb: string; parens: boolean; id: string; title: string }[],
  subtasks: number,
  generated: readonly number[],
  today: number,
): string {
  const first = actions[0];
  if (!first) return "change nothing";
  const n = new Set(actions.map((a) => a.id)).size;
  const shared = actions.every((a) => a.verb === first.verb);
  const text = shared
    ? n === 1
      ? `${first.verb} ${quoted(first.title)}`
      : first.parens
        ? `${first.verb} (${n} items)`
        : `${first.verb} ${n} items`
    : n === 1
      ? `change ${quoted(first.title)}`
      : `change ${n} items`;
  const cascade = subtasks === 0 ? text : `${text} and ${subtasks} ${subtasks === 1 ? "subtask" : "subtasks"}`;
  if (generated.length === 0) return cascade;
  return generated.length === 1 && n === 1 ? `${cascade} · next ${summaryDay(generated[0]!, today)}` : `${cascade} · ${generated.length} next`;
}

/** `auto-archive 3 done items`, `unarchive 1 reopened item`, or both joined by `and` (summary.rs `auto_archive`). */
function autoArchiveSummary(archived: number, healed: number): string {
  const items = (k: number, what: string) => `${k} ${what} ${k === 1 ? "item" : "items"}`;
  const archive = `auto-archive ${items(archived, "done")}`;
  const heal = `unarchive ${items(healed, "reopened")}`;
  return healed === 0 ? archive : archived === 0 ? heal : `${archive} and ${heal}`;
}

function msg(message: MessageInitShape<typeof SubscribeResponseSchema>["message"]): SubscribeResponse {
  return create(SubscribeResponseSchema, { message });
}

/** An error with an ErrorInfo detail, as the daemon sends them. */
/** A view name trimmed, or INVALID_ARGUMENT on `field`, as check_view_name. */
function viewName(name: string, field: string): string {
  const trimmed = rustTrim(name);
  if (trimmed === "") throw badRequest(field, "must not be blank");
  if ([...trimmed].length > 64) throw badRequest(field, "is longer than 64 characters");
  if (/\p{Cc}/u.test(trimmed)) throw badRequest(field, "must not hold a control character");
  return trimmed;
}

/** The longest relative-date offset a stored view takes (MAX_VIEW_OFFSET_DAYS). */
const MAX_VIEW_OFFSET_DAYS = 36_525;

/**
 * A view's filter and sort, as the daemon checks them: an invalid filter,
 * then the sort, then a filter not built yet, then an offset too far for a
 * view. Paths start with `prefix`; either may be left out (not in a mask).
 */
function checkViewFilter(prefix: string, filter: Filter | undefined, sort: readonly SortKey[] | undefined): void {
  const at = (p: FilterProblem): FilterProblem => ({ ...p, path: `${prefix}${p.path}` });
  const problem = filter && checkFilter(filter, undefined);
  if (problem?.kind === "invalid") throw problemError(at(problem));
  const sortProblem = sort && checkSort(sort);
  if (sortProblem) throw problemError(at(sortProblem));
  if (problem) throw problemError(at(problem));
  if (filter) {
    const far = farOffset(filter.allOf, filter.anyOf, "filter");
    if (far) throw badRequest(`${prefix}${far}`, `a view's offset is at most ${MAX_VIEW_OFFSET_DAYS} days`);
  }
}

/** The path of the first relative date further than MAX_VIEW_OFFSET_DAYS. */
function farOffset(allOf: readonly ClauseMessage[], anyOf: readonly ClauseMessage[], path: string): string | undefined {
  for (const [list, clauses] of [["all_of", allOf], ["any_of", anyOf]] as const) {
    for (const [i, c] of clauses.entries()) {
      const here = `${path}.${list}[${i}]`;
      if (c.kind.case === "predicate") {
        for (const [k, o] of c.kind.value.operands.entries()) {
          if (o.kind.case === "relativeDate" && Math.abs(o.kind.value.offsetDays) > MAX_VIEW_OFFSET_DAYS) {
            return `${here}.predicate.operands[${k}].relative_date.offset_days`;
          }
        }
      } else if (c.kind.case === "group") {
        const inner = farOffset(c.kind.value.allOf, c.kind.value.anyOf, `${here}.group`);
        if (inner) return inner;
      }
    }
  }
  return undefined;
}

function errorInfo(code: Code, reason: string, message: string, metadata: Record<string, string>): ConnectError {
  return new ConnectError(message, code, undefined, [{ desc: ErrorInfoSchema, value: { reason, domain: "todo-tui", metadata } }]);
}

/** `message`, prefixed in a batch with the mutation's id path, so a precondition failure names the mutation. */
function inBatch(p: Planned, message: string): string {
  return p.batched ? `${p.idPath}: ${message}` : message;
}

function notFound(id: string): ConnectError {
  return errorInfo(Code.NotFound, "ITEM_NOT_FOUND", `item ${id} not found`, { item_id: id });
}

/**
 * A refused filter or sort as the daemon sends it: INVALID_ARGUMENT naming
 * the path, or UNIMPLEMENTED with ErrorInfo NOT_IMPLEMENTED and the path as
 * "field".
 */
function problemError(p: FilterProblem): ConnectError {
  if (p.kind === "invalid") return badRequest(p.path, p.reason);
  return errorInfo(Code.Unimplemented, "NOT_IMPLEMENTED", `${p.path}: ${p.reason}`, { field: p.path });
}

/** Which Changes a Subscribe asks for: those of its topics, or every one when it names none. */
function topicFilter(topics: readonly Topic[]): (c: Change) => boolean {
  for (const t of topics) {
    if (t === Topic.UNSPECIFIED) throw badRequest("topics", "value must not be UNSPECIFIED");
    if (Topic[t] === undefined) throw badRequest("topics", `unknown enum value ${t}`);
  }
  return (c) => topics.length === 0 || topics.includes(c.topic);
}

/** INVALID_ARGUMENT with one BadRequest violation, as the daemon sends them. */
function badRequest(field: string, description: string): ConnectError {
  return new ConnectError(`${field}: ${description}`, Code.InvalidArgument, undefined, [
    { desc: BadRequestSchema, value: { fieldViolations: [{ field, description }] } },
  ]);
}

/**
 * A relation end that names no Item, or (for an add) a deleted one, named as
 * the request does: its from_id is NOT_FOUND or ITEM_DELETED, its to_id
 * INVALID_ARGUMENT on "relation.to_id".
 */
function relationEnd(asked: Relation, end: string, problem: "missing" | "deleted"): ConnectError {
  if (end === asked.fromId) {
    return problem === "missing"
      ? notFound(end)
      : errorInfo(Code.FailedPrecondition, "ITEM_DELETED", `item ${end} is deleted`, { item_id: end });
  }
  return badRequest("relation.to_id", problem === "missing" ? "no such Item" : "the Item is deleted");
}

const TOO_LONG = `is longer than ${MAX_CAPTURE_BYTES / 1024} KiB`;

/** Whether `text` is over MAX_CAPTURE_BYTES as UTF-8, as protobuf encodes it. */
function overCaptureLimit(text: string): boolean {
  // A UTF-16 code unit is at most 3 bytes of UTF-8.
  return text.length * 3 > MAX_CAPTURE_BYTES && new TextEncoder().encode(text).length > MAX_CAPTURE_BYTES;
}

const ULID = /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/i;

/** `s` in canonical (upper) case, or null when it is not a ULID. */
function canonicalUlid(s: string): string | null {
  return ULID.test(s) ? s.toUpperCase() : null;
}

/** The request id in canonical (upper) case, so every spelling of it replays. */
function requestIdOf(requestId: string): string {
  const id = canonicalUlid(requestId);
  if (id === null) throw badRequest("request_id", "must be a ULID");
  return id;
}

/** CaptureItem's created_via, converted and checked as the daemon does: TUI, CLI, MCP or MOBILE. */
function checkCreatedVia(via: CreatedVia): void {
  if (via === CreatedVia.UNSPECIFIED) throw badRequest("created_via", "value must not be UNSPECIFIED");
  if (CreatedVia[via] === undefined) throw badRequest("created_via", `unknown enum value ${via}`);
  if (via !== CreatedVia.TUI && via !== CreatedVia.CLI && via !== CreatedVia.MCP && via !== CreatedVia.MOBILE) {
    throw badRequest("created_via", "must be TUI, CLI, MCP or MOBILE; the other values are reserved for the daemon");
  }
}

/** The ULID at request field `path`, in canonical (upper) case. */
function idOf(path: string, id: string): string {
  const canonical = canonicalUlid(id);
  if (canonical === null) throw badRequest(path, `invalid ULID ${JSON.stringify(id)}`);
  return canonical;
}

/**
 * The request's relation, with its ids and kind checked, in canonical case,
 * its ends as the request names them; the caller checks that they differ.
 */
function relationOf(relation: Relation | undefined): Relation {
  if (!relation) throw badRequest("relation", "required field is missing");
  const fromId = idOf("relation.from_id", relation.fromId);
  const toId = idOf("relation.to_id", relation.toId);
  if (relation.kind === RelationKind.UNSPECIFIED) throw badRequest("relation.kind", "value must not be UNSPECIFIED");
  if (!(relation.kind in RELATION_WORDS)) throw badRequest("relation.kind", `unknown enum value ${relation.kind}`);
  return create(RelationSchema, { fromId, toId, kind: relation.kind });
}

/** The relation as stored: a RELATES_TO has from_id < to_id. */
function canonical(r: Relation): Relation {
  return r.kind === RelationKind.RELATES_TO && r.toId < r.fromId ? create(RelationSchema, { fromId: r.toId, toId: r.fromId, kind: r.kind }) : r;
}

function relationKey(r: Relation): string {
  return `${r.fromId} ${r.toId} ${r.kind}`;
}

function cmp(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

/** By created_at, then id. */
function byCreation(a: Item, b: Item): number {
  const [x, y] = [a.createdAt, b.createdAt];
  const secs = (x?.seconds ?? 0n) - (y?.seconds ?? 0n);
  if (secs !== 0n) return secs < 0n ? -1 : 1;
  const nanos = (x?.nanos ?? 0) - (y?.nanos ?? 0);
  return nanos !== 0 ? nanos : cmp(a.id, b.id);
}

/** `prev` with the fields `init` names replaced. */
function merged(prev: Item, init: MessageInitShape<typeof ItemSchema>): Item {
  const patch = create(ItemSchema, init) as unknown as Fields;
  const item = { ...prev } as unknown as Fields;
  for (const k of Object.keys(init)) item[k] = patch[k];
  return item as unknown as Item;
}

/** One mutation of a BatchMutate, converted as the daemon does: errors name `mutations[i].<case>.<path>`. */
function planMutation(m: Mutation, prefix: string, today: number): Planned {
  const k = m.kind;
  if (k.case === undefined) throw badRequest(`${prefix}kind`, "required field is missing");
  const inner = `${prefix}${k.case.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`)}.`;
  if (k.value.requestId !== "") throw badRequest(`${inner}request_id`, "must be empty in a batch: the batch's request_id covers it");
  switch (k.case) {
    case "updateItem":
      return planUpdate(k.value, inner, today);
    case "setStatus":
      return planSetStatus(k.value, inner);
    case "archiveItem":
      return planItemOp("archive", k.value, inner);
    case "unarchiveItem":
      return planItemOp("unarchive", k.value, inner);
    case "deleteItem":
      return planItemOp("delete", k.value, inner);
    case "undeleteItem":
      return planItemOp("undelete", k.value, inner);
    case "setRecurrence":
      return planSetRecurrence(k.value, inner, today);
  }
}

/**
 * SetRecurrence, converted as set_recurrence_from_proto does: errors in the
 * rule are at `rule.<oneof case>`. Each call names a fresh series, which
 * the Item takes only if it does not recur yet, so every Item of a batch
 * gets its own; `today` places a fixed rule's first occurrence on an Item
 * without a due date.
 */
function planSetRecurrence(req: SetRecurrenceRequest, prefix: string, today: number): Planned {
  const idPath = `${prefix}id`;
  const id = idOf(idPath, req.id);
  let rule: Rule | undefined;
  if (req.rule !== undefined) {
    const read = ruleFromProto(`${prefix}rule`, req.rule);
    if ("reason" in read) throw badRequest(read.path, read.reason);
    rule = read.rule;
  }
  return {
    kind: "setRecurrence",
    id,
    expectedRevision: req.expectedRevision,
    prefix,
    idPath,
    batched: prefix !== "",
    recurrence: { rule, seriesId: ulid(), today },
  };
}

/**
 * The date at request field `field`, as date_from_proto reads it: a year of
 * 1 to 9999 and a real date, with jiff's wording.
 */
function dateArg(field: string, d: { year: number; month: number; day: number }): number {
  const bad = (reason: string) => badRequest(field, reason);
  if (!(d.year >= 1 && d.year <= 9999)) throw bad(`year ${d.year} is not in 1..=9999`);
  // Rust's i8::try_from.
  if ([d.month, d.day].some((n) => n < -128 || n > 127)) throw bad("out of range integral type conversion attempted");
  if (!(d.month >= 1 && d.month <= 12)) throw bad("parameter 'month' is not in the required range of 1..=12");
  if (d.day < 1) throw bad("parameter 'day' is not in the required range of 1..=31");
  if (!isDate(d.year, d.month, d.day)) {
    const last = [31, 30, 29, 28].find((n) => isDate(d.year, d.month, n))!;
    const pad = (n: number, w: number) => String(n).padStart(w, "0");
    throw bad(`parameter 'day' for \`${pad(d.year, 4)}-${pad(d.month, 2)}\` is invalid, must be in range \`1..=${last}\``);
  }
  return dayOf(d);
}

/** UpdateItem; `today` is the day a `waiting` without `since` is waiting since. */
function planUpdate(req: UpdateItemRequest, prefix: string, today: number): Planned {
  const idPath = `${prefix}item.id`;
  const id = idOf(idPath, req.item?.id ?? "");
  return { kind: "update", id, expectedRevision: req.expectedRevision, prefix, idPath, batched: prefix !== "", mask: maskOf(req, prefix, today) };
}

function planSetStatus(req: { id: string; status: Status; expectedRevision?: bigint | undefined }, prefix: string): Planned {
  const idPath = `${prefix}id`;
  const id = idOf(idPath, req.id);
  if (req.status === Status.UNSPECIFIED) throw badRequest(`${prefix}status`, "value must not be UNSPECIFIED");
  if (!(req.status in STATUS_VERBS)) throw badRequest(`${prefix}status`, `unknown enum value ${req.status}`);
  return { kind: "setStatus", id, expectedRevision: req.expectedRevision, prefix, idPath, batched: prefix !== "", status: req.status };
}

function planItemOp(kind: MutationKind, req: { id: string; expectedRevision?: bigint | undefined }, prefix: string): Planned {
  const idPath = `${prefix}id`;
  return { kind, id: idOf(idPath, req.id), expectedRevision: req.expectedRevision, prefix, idPath, batched: prefix !== "" };
}

/** Item fields that change only through SetStatus. */
const STATUS_PATHS = new Set(["status", "status_category", "resolution_kind", "todo_refinement"]);

const FIELDS_BY_NAME = new Map(ItemSchema.fields.map((f) => [f.name, f]));

/** The Core fields (design 3.2), by local name, with their paths: the tracker's on a linked Item. */
const CORE_PATHS: Record<string, string> = {
  title: "title",
  description: "description",
  itemType: "item_type",
  statusCategory: "status_category",
  resolutionKind: "resolution_kind",
  priority: "priority",
  assignee: "assignee",
  labels: "labels",
  components: "components",
  fixVersions: "fix_versions",
  dueDate: "due_date",
  parentId: "parent_id",
  storyPoints: "story_points",
  originalEstimate: "original_estimate",
  remainingEstimate: "remaining_estimate",
};
const CORE_FIELDS = new Set(Object.keys(CORE_PATHS));
/** The Core fields a push sends (ADR-0007, ADR-0009), as todo_core's `pushed`. */
const PUSHED_FIELDS = new Set(["title", "description", "priority", "labels", "dueDate", "statusCategory", "resolutionKind"]);

/** The Item fields whose own fields are mask paths too ("waiting.reason"), as ItemField names them. */
const NESTED_PATHS = new Set(["waiting", "archived", "remote_link", "recurrence", "staleness"]);

/**
 * Whether `path` names an Item field, or a field of one of the NESTED_PATHS
 * messages that ItemField names (its number is the parent's × 1000 plus its
 * own), as the daemon reads a path: `recurrence.rule` is no ItemField, so
 * no path.
 */
function knownPath(path: string): boolean {
  const [top, sub, ...deeper] = path.split(".");
  const field = FIELDS_BY_NAME.get(top!);
  if (field === undefined || deeper.length > 0) return false;
  if (sub === undefined) return true;
  if (!NESTED_PATHS.has(top!) || field.fieldKind !== "message") return false;
  const nested = field.message.fields.find((f) => f.name === sub);
  return nested !== undefined && ItemField[field.number * 1000 + nested.number] !== undefined;
}

/**
 * The update mask's paths and the request's values for them by path (for a
 * nested waiting path, that field of the request's waiting), converted as
 * the daemon converts the request, before it looks at the Item: it rejects
 * a missing or empty mask, "*", relations, an unknown path, a parent_id
 * that is not a ULID and a waiting without `on`, and a waiting without
 * `since` is waiting since `today` (read, like every value, only up to the
 * first repeated path).
 */
function maskOf(req: UpdateItemRequest, prefix: string, today: number): { paths: readonly string[]; values: Fields } {
  if (req.updateMask === undefined) throw badRequest(`${prefix}update_mask`, "required field is missing");
  const paths = req.updateMask.paths;
  if (paths.length === 0) throw badRequest(`${prefix}update_mask`, "must not be empty");
  const source = (req.item ?? create(ItemSchema)) as unknown as Fields;
  const values: Fields = {};
  let repeated = false;
  for (const [at, path] of paths.entries()) {
    if (path === "*") throw badRequest(`${prefix}update_mask`, '"*" is not supported');
    if (path === "relations") throw badRequest(`${prefix}item.relations`, "changes only through AddRelation and RemoveRelation");
    if (!knownPath(path)) throw badRequest(`${prefix}update_mask`, `unknown field path ${JSON.stringify(path)}`);
    repeated ||= paths.indexOf(path) < at;
    if (repeated) continue;
    const [top, sub] = path.split(".");
    const field = FIELDS_BY_NAME.get(top!)!.localName;
    const value = source[field];
    if (sub !== undefined) {
      const waiting = top === "waiting" ? (value as Waiting | undefined) : undefined;
      // An unset reason is "", as proto3 reads it.
      values[path] = sub === "reason" ? (waiting?.reason ?? "") : (waiting as unknown as Fields | undefined)?.[WAITING_FIELDS.get(sub) ?? ""];
    } else if (field === "parentId" && typeof value === "string") {
      values[path] = idOf(`${prefix}item.parent_id`, value);
    } else if (field === "waiting" && value !== undefined) {
      const waiting = value as Waiting;
      if (waiting.on === undefined) throw badRequest(`${prefix}item.waiting.on`, "required field is missing");
      const out = clone(WaitingSchema, waiting);
      out.since ??= create(DateSchema, protoDate(today));
      values[path] = out;
    } else {
      values[path] = value;
    }
  }
  return { paths, values };
}

/** Waiting's fields' localNames by name. */
const WAITING_FIELDS = new Map(WaitingSchema.fields.map((f) => [f.name, f.localName]));

/**
 * The top-level Item fields UpdateItem writes; the others are status
 * fields (SetStatus), archived (ArchiveItem) or read-only.
 */
const WRITABLE = new Set([
  "title",
  "description",
  "item_type",
  "priority",
  "assignee",
  "labels",
  "components",
  "fix_versions",
  "due_date",
  "parent_id",
  "story_points",
  "original_estimate",
  "remaining_estimate",
  "waiting",
  "defer_until",
  "private_notes",
  "focus_week",
  "local_priority_override",
  "energy",
]);

/** What todo_core takes for a line break, which a title must not have. */
const LINE_BREAK = /[\n\r\v\f\u0085\u2028\u2029]/;

/** What a label must not contain: Unicode White_Space or a control character, as todo_core checks. */
const BAD_LABEL = /[\p{White_Space}\p{Cc}]/u;

/** `on` as the daemon stores a waiting's person: free text trimmed, and then not empty. */
function waitingOn(on: PersonRef, prefix: string): PersonRef {
  if (on.ref.case !== "freeText") return on;
  const text = rustTrim(on.ref.value);
  if (text === "") throw badRequest(`${prefix}item.waiting.on`, "free text must not be empty");
  return create(PersonRefSchema, { ref: { case: "freeText", value: text } });
}

/**
 * `waiting` with its field at `path` (e.g. "waiting.reason") set to `value`,
 * as the daemon sets a nested waiting path: only that field changes, `on`
 * cannot be cleared (clear waiting instead) and `since` cannot be cleared.
 */
function withWaitingField(waiting: Waiting, path: string, value: unknown, prefix: string): Waiting {
  const out = clone(WaitingSchema, waiting);
  switch (path) {
    case "waiting.on":
      if (value === undefined) throw badRequest(`${prefix}item.waiting.on`, "clear waiting instead");
      out.on = waitingOn(value as PersonRef, prefix);
      break;
    case "waiting.reason":
      out.reason = value as string;
      break;
    case "waiting.since":
      if (value === undefined) throw badRequest(`${prefix}item.waiting.since`, "cannot be cleared");
      out.since = value as Waiting["since"];
      break;
    default:
      out.followUpOn = value as Waiting["followUpOn"];
  }
  return out;
}


/** A Suggestion waits while it is pending or partly applied. */
const waits = (s: Suggestion): boolean => s.state === SuggestionState.PENDING || s.state === SuggestionState.PARTIALLY_APPLIED;

/** A Suggestion's state from its patches, as suggestion.rs settles it. */
function settledState(s: Suggestion): SuggestionState {
  const count = (st: PatchState) => s.patches.filter((p) => p.state === st).length;
  const [pending, applied, rejected] = [count(PatchState.PENDING), count(PatchState.APPLIED), count(PatchState.REJECTED)];
  if (pending && applied) return SuggestionState.PARTIALLY_APPLIED;
  if (pending) return SuggestionState.PENDING;
  if (applied) return SuggestionState.APPLIED;
  if (rejected) return SuggestionState.REJECTED;
  return SuggestionState.EXPIRED;
}

/** The Item field's update path ("title", "due_date"), by its ItemField number. */
const PATH_OF_NUMBER = new Map(ItemSchema.fields.map((f) => [f.number, f.name]));

/** The Item field's localName, by its ItemField number. */
const localOf = (n: ItemField): string => ItemSchema.fields.find((f) => f.number === n)?.localName ?? "";

/** A patch value as the Item field holds it: cleared is unset. */
function valueOfField(_field: ItemField, v: FieldValue | undefined): unknown {
  const k = v?.kind;
  if (!k || k.case === "cleared" || k.case === undefined) return undefined;
  if (k.case === "stringSet") return [...k.value.values];
  return k.value;
}

/** The status a status pair sets on `item`, as suggestion.rs's target_status. */
function statusFor(item: Item, category: StatusCategory, kind: ResolutionKind | undefined): Status {
  switch (category) {
    case StatusCategory.TODO:
      return item.todoRefinement === TodoRefinement.NEXT ? Status.NEXT : item.todoRefinement === TodoRefinement.SOMEDAY ? Status.SOMEDAY : Status.INBOX;
    case StatusCategory.IN_PROGRESS:
      return Status.IN_PROGRESS;
    default:
      return kind === ResolutionKind.CANCELLED ? Status.CANCELLED : Status.DONE;
  }
}

/** An apply's toast text, as suggestion.rs's summary words it. */
function applySummary(actions: readonly { id: string; title: string }[]): string {
  const ids = new Set(actions.map((a) => a.id));
  const [first] = actions;
  if (!first) return "apply nothing";
  return ids.size === 1 ? `apply suggestion ${quoted(first.title)}` : `apply suggestions (${ids.size} items)`;
}
