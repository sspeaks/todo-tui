import { type MessageInitShape, clone, create, equals } from "@bufbuild/protobuf";
import { type Timestamp, TimestampSchema } from "@bufbuild/protobuf/wkt";
import { Code, ConnectError, type Transport, createRouterTransport } from "@connectrpc/connect";
import { BadRequestSchema, ErrorInfoSchema } from "./gen/google/rpc/error_details_pb.js";
import { DateSchema } from "./gen/google/type/date_pb.js";
import { type MutationResultSchema, type PersonRef, PersonRefSchema, Topic } from "./gen/todo/v1/common_pb.js";
import { ItemField } from "./gen/todo/v1/field_pb.js";
import { type Filter, FilterSchema, type OperandSchema, Operator, RelativeDate_Anchor } from "./gen/todo/v1/filter_pb.js";
import {
  ArchiveReason,
  ArchivedSchema,
  CreatedVia,
  type Item,
  ItemSchema,
  ResolutionKind,
  Status,
  StatusCategory,
  TodoRefinement,
  type Waiting,
  WaitingSchema,
} from "./gen/todo/v1/item_pb.js";
import {
  type AddRelationRequest,
  type CaptureDraft,
  type CaptureItemRequest,
  type GetStatusCountsRequest,
  type GetStatusCountsResponseSchema,
  ItemService,
  type ListItemsRequest,
  type ListItemsResponseSchema,
  type Mutation,
  type ParseCaptureRequest,
  type ParseDateRequest,
  type ParseDateResponseSchema,
  type RedoRequest,
  type RemoveRelationRequest,
  type UndoRequest,
  type UpdateItemRequest,
} from "./gen/todo/v1/item_service_pb.js";
import { OpKind, type OpSummary, OpSummarySchema, type UndoSkip, UndoSkipReason, UndoSkipSchema } from "./gen/todo/v1/op_pb.js";
import { type Relation, RelationKind, RelationSchema } from "./gen/todo/v1/relation_pb.js";
import { SystemService } from "./gen/todo/v1/system_service_pb.js";
import { type View, ViewSchema } from "./gen/todo/v1/view_pb.js";
import { ViewService } from "./gen/todo/v1/view_service_pb.js";
import { type Change, ChangeSchema, type SubscribeResponse, SubscribeResponseSchema, WatchService } from "./gen/todo/v1/watch_service_pb.js";
import { parseCaptureText } from "./capture.js";
import { dayIn, epochMs, parseDateExpr, protoDate, rustTrim } from "./dates.js";
import { defaultViews } from "./default-views.js";
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
import { ulid } from "./ulid.js";

type Waiter = (msg: SubscribeResponse | Error) => void;

type MutationResult = MessageInitShape<typeof MutationResultSchema>;

/** An Item's fields by localName. */
type Fields = Record<string, unknown>;

interface MutationResponse {
  item: Item;
  result: MutationResult;
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

type MutationKind = "update" | "setStatus" | "archive" | "unarchive" | "delete" | "undelete";

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
  /** UpdateItem: the mask's paths, in order, and the request's values for them, by path. */
  mask?: { paths: readonly string[]; values: Fields };
  /** SetStatus: the target. */
  status?: Status;
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

/** The stored fields behind the derived status, in SetStatus event order (design 3.4). */
const TRIPLE: readonly string[] = ["statusCategory", "resolutionKind", "todoRefinement"];

type Triple = Pick<Item, "statusCategory" | "resolutionKind" | "todoRefinement">;

const INBOX_TRIPLE: Triple = { statusCategory: StatusCategory.TODO, resolutionKind: undefined, todoRefinement: TodoRefinement.INBOX };

const FIELDS = new Map(ItemSchema.fields.map((f) => [f.localName, f]));

/** Item fields that put's system op does not record events for. */
const UNTRACKED = new Set(["revision", "updatedAt", "status", "relations"]);

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
 *   read, for the request's today (in {@link FakeDaemonOptions.timeZone}).
 * - ListItems with the whole filter language (filter-eval.ts, ported from
 *   todo_core::filter): the implicit rules, the checks with the daemon's
 *   error paths and its INVALID / UNIMPLEMENTED split, relative dates,
 *   sorts with the created_at, id tie-break, keyset page tokens bound to the
 *   effective filter and sort that keep the first page's today, and view_id
 *   (VIEW_NOT_FOUND). GetStatusCounts, and ParseDate with the capture date
 *   grammar.
 * - The six default views (todo_core::view::defaults()), seeded first as
 *   the store seeds them, each with a revision and a TOPIC_VIEW Change;
 *   ListViews. Views are read-only, as the daemon's are.
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
 *   restores nothing drops its entry; an undone capture is soft-deleted; replays return the original
 *   entry and skips. GetUndoState, and OpSummary texts and error messages
 *   as the daemon words them.
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
 * What it does not model: other field values are not checked (enums, dates,
 * numbers, estimates, focus_week, people, and the tracker-only fields of an
 * unlinked Item are taken as given). An op records a changed top-level
 * field as one event, so a nested waiting path records, and its undo
 * restores, the whole `waiting`. Staleness signals are not maintained or
 * restored (`updated_at` is set on every op that touches an Item); request
 * ids never expire. MATCHES approximates the full-text index (lower case,
 * accents stripped, runs of letters and digits), so it agrees on ASCII and
 * Latin text but not on every script or symbol.
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
  apiMinor = 5;
  readonly calls = { listItems: 0, listViews: 0, getStatusCounts: 0, subscribe: [] as (bigint | undefined)[] };
  readonly transport: Transport;
  readonly now: () => Date;
  readonly timeZone: string;
  private readonly watchers = new Set<Waiter>();
  /** Requests by request_id (upper case), for replays. */
  private readonly requests = new Map<string, Recorded>();
  /** Captures by spool_id (upper case), for replays. */
  private readonly spools = new Map<string, Recorded>();
  /** Every relation, by relationKey. */
  private readonly relations = new Map<string, Relation>();
  private nextId = 0;
  /** The last write time handed out, in nanoseconds. */
  private lastWrite = -(2n ** 63n);

  constructor(initial: MessageInitShape<typeof ItemSchema>[] = [], options: FakeDaemonOptions = {}) {
    this.now = options.now ?? (() => new Date());
    this.timeZone = options.timeZone ?? Intl.DateTimeFormat().resolvedOptions().timeZone;
    this.seedViews();
    for (const init of initial) this.put(init);
    this.transport = createRouterTransport(({ service }) => {
      service(SystemService, {
        getServerInfo: () => {
          this.check();
          return { daemonVersion: "fake", apiMinor: this.apiMinor, capabilities: [] };
        },
        shutdown: () => ({}),
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
        updateItem: (req) => this.single("UpdateItem", OpKind.UPDATE, req.requestId, () => planUpdate(req, "", this.today())),
        setStatus: (req) => this.single("SetStatus", OpKind.SET_STATUS, req.requestId, () => planSetStatus(req, "")),
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
          const planned = req.mutations.map((m, i) => planMutation(m, `mutations[${i}].`, today));
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
      service(ViewService, {
        listViews: () => {
          this.check();
          this.calls.listViews++;
          return { views: this.tabOrder(), revision: this.revision };
        },
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
                for (const c of self.changes) if (c.revision > since && wants(c)) yield msg({ case: "change", value: c });
              }
            }
            yield msg({ case: "caughtUp", value: { revision: self.revision } });
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
    this.items.set(id, item);
    this.log({ kind: OpKind.UNSPECIFIED, actor: "system", summary: "", events: prev ? diff(prev, item) : [{ kind: "create", itemId: id }] });
    this.publish({ revision: this.revision, topic: Topic.ITEM, id, resource: { case: "item", value: item } });
    return item;
  }

  /** put on an existing Item. */
  update(id: string, patch: MessageInitShape<typeof ItemSchema>): Item {
    return this.put({ ...patch, id });
  }

  /** Removes an Item outright (not a soft delete) and publishes a deleted Change. */
  remove(id: string): void {
    this.revision++;
    this.items.delete(id);
    this.publish({ revision: this.revision, topic: Topic.ITEM, id, deleted: true });
  }

  /** Changes a saved view and publishes its Change, as an edit will (views are read-only over the API until M6). */
  updateView(id: string, patch: MessageInitShape<typeof ViewSchema>): View {
    const prev = this.views.get(id);
    if (!prev) throw new Error(`no view ${id}`);
    const fields = create(ViewSchema, patch) as unknown as Fields;
    const view = clone(ViewSchema, prev);
    for (const k of Object.keys(patch)) (view as unknown as Fields)[k] = fields[k];
    Object.assign(view, { id, revision: ++this.revision, updatedAt: this.stamp() });
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

  /** Fails every open Subscribe stream, as if the connection dropped. */
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
    for (const item of items) {
      this.items.set(item.id, this.settle(item));
      for (const r of item.relations) this.relations.set(relationKey(r), clone(RelationSchema, r));
    }
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
      labels: draft.labels,
      ...withStatus(INBOX_TRIPLE, draft.status),
      status: draft.status,
      ...(draft.dueDate !== undefined && { dueDate: draft.dueDate }),
      ...(draft.waitingOn !== undefined && { waiting: { on: draft.waitingOn, reason: "", since: protoDate(day) } }),
      ...(parentId !== undefined && { parentId }),
    });
    item.revision = ++this.revision;
    this.items.set(item.id, this.settle(item));
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
    const { items, result } = this.mutate(rpc, kind, key, [plan()]);
    return { item: items[0]!, result };
  }

  /**
   * Runs the converted `mutations` in order as one op, all or nothing, in
   * the store's order: a replay returns the original result with the Items'
   * current state; otherwise each Item must exist, be at its
   * expected_revision (as it was before the op) and, unless the mutation is
   * a delete or undelete, not be deleted. A delete takes the live
   * descendants along, an undelete the descendants its delete took; no
   * Item the op undeleted may end it live under a deleted parent.
   */
  private mutate(rpc: string, kind: OpKind, requestId: string, planned: Planned[]): { items: Item[]; result: MutationResult } {
    // A single-item op names its Item, so the same request id on another Item
    // is a different request; a batch is matched by its RPC alone.
    const recorded = this.recorded(rpc, requestId, kind === OpKind.BATCH ? undefined : planned[0]!.id);
    if (recorded) return { items: planned.map((p) => this.present(this.get(p.id), this.today())), result: recorded.result };
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
    const actions = planned.map((p, i) => ({ verb: verbOf(p), status: p.kind === "setStatus", id: p.id, title: batch.items[slots[i]!]!.title }));
    const op = this.commit(batch, kind, mutationSummary(actions, subtasks.size), now);
    const result = this.record(rpc, requestId, planned[0]!.id, op);
    return { items: slots.map((s) => this.present(batch.items[s]!, this.today())), result };
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
    const steps = inverse(this.sourceOf(entry, redo).events, now);
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
    entry.undoState = redo ? "done" : "undone";
    const items = itemIdsOf(batch.events).map((id) => batch.items[batch.slots.get(id)!]!);
    return { result: this.record(rpc, requestId, "", op), entry: summarized, items, skipped };
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
    return { batch, skipped, changed, relations, broken };
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
  private commit(batch: Working, kind: OpKind, summary: string, now: Timestamp, extra: Pick<FakeOp, "entryOp" | "skipped"> = {}): FakeOp {
    for (const slot of batch.touched()) {
      const item = batch.items[slot]!;
      item.updatedAt = now;
      item.revision = ++this.revision;
      this.items.set(item.id, this.settle(item));
      this.publish({ revision: item.revision, topic: Topic.ITEM, id: item.id, resource: { case: "item", value: item } });
    }
    return this.log({ kind, actor: "user", summary, events: batch.events, ...extra });
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
    for (const w of [...this.watchers]) w(msg({ case: "change", value: change }));
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
  /** Slots that get a revision: the ones with events, and relation ends. */
  private readonly dirty = new Set<number>();
  /** Items this op soft-deleted and has not undeleted since. */
  private readonly deletedHere = new Set<string>();

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

  /** Takes `next` as the slot's Item without an event of its own (a relation end). */
  touch(slot: number, next: Item): void {
    this.items[slot] = next;
    this.dirty.add(slot);
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
        if (field === "parentId" && value === item.id) throw badRequest(where, "an Item cannot be its own parent");
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
    case "setStatus":
      return p.status === undefined || deriveStatus(item) === p.status ? {} : withStatus(item, p.status);
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
    default:
      return p.kind;
  }
}

/** `"<title>"`, cut to MAX_TITLE_CHARS characters with `…`. */
function quoted(title: string): string {
  const chars = [...title];
  return chars.length <= MAX_TITLE_CHARS ? `"${title}"` : `"${chars.slice(0, MAX_TITLE_CHARS - 1).join("")}…"`;
}

/**
 * The summary of a mutate op from its mutations in request order (titles as
 * after the op) and the number of descendants a cascade took along that no
 * mutation names: `mark done "x"`, `mark done (3 items)`, `edit 3 items`,
 * `change 3 items`, `delete "x" and 2 subtasks`.
 */
function mutationSummary(actions: readonly { verb: string; status: boolean; id: string; title: string }[], subtasks: number): string {
  const first = actions[0];
  if (!first) return "change nothing";
  const n = new Set(actions.map((a) => a.id)).size;
  const shared = actions.every((a) => a.verb === first.verb);
  const text = shared
    ? n === 1
      ? `${first.verb} ${quoted(first.title)}`
      : first.status
        ? `${first.verb} (${n} items)`
        : `${first.verb} ${n} items`
    : n === 1
      ? `change ${quoted(first.title)}`
      : `change ${n} items`;
  return subtasks === 0 ? text : `${text} and ${subtasks} ${subtasks === 1 ? "subtask" : "subtasks"}`;
}

function msg(message: MessageInitShape<typeof SubscribeResponseSchema>["message"]): SubscribeResponse {
  return create(SubscribeResponseSchema, { message });
}

/** An error with an ErrorInfo detail, as the daemon sends them. */
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

/** CaptureItem's created_via, converted and checked as the daemon does: TUI, CLI or MCP. */
function checkCreatedVia(via: CreatedVia): void {
  if (via === CreatedVia.UNSPECIFIED) throw badRequest("created_via", "value must not be UNSPECIFIED");
  if (CreatedVia[via] === undefined) throw badRequest("created_via", `unknown enum value ${via}`);
  if (via !== CreatedVia.TUI && via !== CreatedVia.CLI && via !== CreatedVia.MCP) {
    throw badRequest("created_via", "must be TUI, CLI or MCP; the other values are reserved for the daemon");
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
  }
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

/** The Item fields whose own fields are mask paths too ("waiting.reason"), as ItemField names them. */
const NESTED_PATHS = new Set(["waiting", "archived", "remote_link", "recurrence", "staleness"]);

/** Whether `path` names an Item field, or a field of one of the NESTED_PATHS messages. */
function knownPath(path: string): boolean {
  const [top, sub, ...deeper] = path.split(".");
  const field = FIELDS_BY_NAME.get(top!);
  if (field === undefined || deeper.length > 0) return false;
  if (sub === undefined) return true;
  return NESTED_PATHS.has(top!) && field.fieldKind === "message" && field.message.fields.some((f) => f.name === sub);
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

