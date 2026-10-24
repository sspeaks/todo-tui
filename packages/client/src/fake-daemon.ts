import { type MessageInitShape, create, equals } from "@bufbuild/protobuf";
import { timestampNow } from "@bufbuild/protobuf/wkt";
import { Code, ConnectError, type Transport, createRouterTransport } from "@connectrpc/connect";
import { BadRequestSchema, ErrorInfoSchema } from "./gen/google/rpc/error_details_pb.js";
import type { DateSchema } from "./gen/google/type/date_pb.js";
import { type MutationResultSchema, Topic } from "./gen/todo/v1/common_pb.js";
import { ArchiveReason, type Item, ItemSchema, Status } from "./gen/todo/v1/item_pb.js";
import { ItemService, type ListItemsRequest, type UpdateItemRequest } from "./gen/todo/v1/item_service_pb.js";
import { SystemService } from "./gen/todo/v1/system_service_pb.js";
import { type Change, ChangeSchema, type SubscribeResponse, SubscribeResponseSchema, WatchService } from "./gen/todo/v1/watch_service_pb.js";
import { MAX_CAPTURE_BYTES } from "./limits.js";
import { ulid } from "./ulid.js";

type Waiter = (msg: SubscribeResponse | Error) => void;

type MutationResult = MessageInitShape<typeof MutationResultSchema>;

interface MutationResponse {
  item: Item;
  result: MutationResult;
}

/** A request remembered by its request_id, for replays. */
interface Recorded {
  /** The RPC it was. */
  rpc: string;
  /** The Item it targeted (or created). */
  id: string;
  result: MutationResult;
}

/** A soft-deleted Item takes no other mutation (item_service.proto). */
const ON_DELETED = new Set(["DeleteItem", "UndeleteItem"]);

/**
 * An in-memory daemon over createRouterTransport for tests: ItemService,
 * WatchService with replay / CaughtUp / ResyncRequired, and
 * SystemService.GetServerInfo. It follows the daemon's ItemService
 * conventions: ULID request ids and Item ids, request_id replay (the original
 * result with the current state) and reuse checks, expected_revision,
 * soft-deleted Items, mutations that change nothing, and ListItems' implicit
 * rules (deleted, archived, done and deferred Items hidden unless asked
 * for), sorted by id. It does not parse captures (the title is the text) or
 * check field values and read-only fields, except the 64 KiB limit on capture
 * text and titles: SetStatus sets only the derived status, and UpdateItem
 * copies the top-level fields its mask names (status, archived and relations
 * paths are refused). BatchMutate, ParseCapture, Undo, Redo, GetUndoState and
 * the relation RPCs are not implemented.
 */
export class FakeDaemon {
  revision = 0n;
  readonly items = new Map<string, Item>();
  readonly changes: Change[] = [];
  /** Subscribing with since_revision below this yields ResyncRequired. */
  replayFloor = 0n;
  /** When true every RPC fails with UNAVAILABLE. */
  down = false;
  pageSize = 1000;
  /** Runs before every ListItems; throw from it to fail that call. */
  onListItems: ((req: ListItemsRequest) => void) | undefined;
  apiMinor = 2;
  readonly calls = { listItems: 0, subscribe: [] as (bigint | undefined)[] };
  readonly transport: Transport;
  private readonly watchers = new Set<Waiter>();
  /** Requests by request_id (upper case), for replays. */
  private readonly requests = new Map<string, Recorded>();
  private nextId = 0;

  constructor(initial: MessageInitShape<typeof ItemSchema>[] = []) {
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
        captureItem: (req) => {
          this.check();
          const requestId = requestIdOf(req.requestId);
          if (overCaptureLimit(req.text)) throw badRequest("text", TOO_LONG);
          if (req.text.trim() === "") throw badRequest("text", "must not be empty");
          const recorded = this.recorded("CaptureItem", requestId);
          if (recorded) return { item: this.get(recorded.id), result: recorded.result };
          const item = this.put({ title: req.text, createdVia: req.createdVia });
          return { item, result: this.record("CaptureItem", requestId, item.id) };
        },
        listItems: (req) => {
          this.check();
          this.calls.listItems++;
          this.onListItems?.(req);
          const f = req.filter;
          const today = localToday();
          const all = [...this.items.values()]
            .filter((i) => f?.includeDeleted || i.deletedAt === undefined)
            .filter((i) => f?.includeArchived || i.archived === undefined)
            .filter((i) => f?.includeDone || (i.status !== Status.DONE && i.status !== Status.CANCELLED))
            .filter((i) => f?.includeDeferred || i.deferUntil === undefined || dateKey(i.deferUntil) <= today)
            .sort((a, b) => (a.id < b.id ? -1 : 1));
          const size = req.pageSize > 0 ? req.pageSize : this.pageSize;
          const offset = req.pageToken ? Number(req.pageToken) : 0;
          const end = offset + size;
          return {
            items: all.slice(offset, end),
            nextPageToken: end < all.length ? String(end) : "",
            revision: this.revision,
          };
        },
        getItem: (req) => {
          this.check();
          return { item: this.get(idOf("id", req.id)), revision: this.revision };
        },
        updateItem: (req) =>
          this.mutate("UpdateItem", req, "item.id", req.item?.id ?? "", () => {
            const patch = maskedFields(req);
            return (item) => {
              if (patch.title !== undefined && overCaptureLimit(patch.title.trim())) throw badRequest("item.title", TOO_LONG);
              return equals(ItemSchema, merged(item, patch), item) ? undefined : patch;
            };
          }),
        setStatus: (req) =>
          this.mutate("SetStatus", req, "id", req.id, () => (item) => (item.status === req.status ? undefined : { status: req.status })),
        archiveItem: (req) =>
          this.mutate("ArchiveItem", req, "id", req.id, () => (item) =>
            item.archived ? undefined : { archived: { at: timestampNow(), reason: ArchiveReason.USER } },
          ),
        unarchiveItem: (req) =>
          this.mutate("UnarchiveItem", req, "id", req.id, () => (item) => (item.archived ? { archived: undefined } : undefined)),
        deleteItem: (req) =>
          this.mutate("DeleteItem", req, "id", req.id, () => (item) => (item.deletedAt ? undefined : { deletedAt: timestampNow() })),
        undeleteItem: (req) =>
          this.mutate("UndeleteItem", req, "id", req.id, () => (item) => (item.deletedAt ? { deletedAt: undefined } : undefined)),
      });
      const self = this;
      service(WatchService, {
        async *subscribe(req, ctx) {
          self.check();
          self.calls.subscribe.push(req.sinceRevision);
          const queue: (SubscribeResponse | Error)[] = [];
          let wake = () => {};
          const waiter: Waiter = (m) => {
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
                for (const c of self.changes) if (c.revision > since) yield msg({ case: "change", value: c });
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

  /** Creates or replaces an item and publishes a Change. */
  put(init: MessageInitShape<typeof ItemSchema>): Item {
    this.revision++;
    const id = init.id || `01FAKE${String(++this.nextId).padStart(20, "0")}`;
    const prev = this.items.get(id);
    const item = prev ? merged(prev, init) : create(ItemSchema, { status: Status.INBOX, createdAt: timestampNow(), ...init });
    item.id = id;
    item.revision = this.revision;
    this.items.set(id, item);
    this.publish({ revision: this.revision, topic: Topic.ITEM, id, resource: { case: "item", value: item } });
    return item;
  }

  update(id: string, patch: MessageInitShape<typeof ItemSchema>): Item {
    return this.put({ ...patch, id });
  }

  remove(id: string): void {
    this.revision++;
    this.items.delete(id);
    this.publish({ revision: this.revision, topic: Topic.ITEM, id, deleted: true });
  }

  /** Fails every open Subscribe stream, as if the connection dropped. */
  dropStreams(code = Code.Unavailable): void {
    for (const w of [...this.watchers]) w(new ConnectError("stream dropped", code));
  }

  /**
   * Replaces the whole state with an older snapshot, as `todo restore` does:
   * the revision goes back to `revision` and the change log is emptied.
   */
  restore(revision: bigint, items: Item[]): void {
    this.revision = revision;
    this.items.clear();
    for (const item of items) this.items.set(item.id, item);
    this.truncateChanges();
  }

  /** Forgets the change log, so older since_revision values need a resync. */
  truncateChanges(): void {
    this.changes.length = 0;
    this.replayFloor = this.revision;
  }

  /**
   * Runs the single-item mutation `rpc` on the Item `rawId` (the request
   * field `idPath`), in the daemon's order: the request id and Item id are
   * checked, then `prepare` checks the rest of the request and returns the
   * change, then a replay returns the original result with the current
   * state; otherwise the Item must exist, be at expected_revision and, unless
   * `rpc` is Delete or UndeleteItem, not be deleted. A change of `undefined`
   * changes nothing and allocates no revision.
   */
  private mutate(
    rpc: string,
    req: { requestId: string; expectedRevision?: bigint | undefined },
    idPath: string,
    rawId: string,
    prepare: () => (item: Item) => MessageInitShape<typeof ItemSchema> | undefined,
  ): MutationResponse {
    this.check();
    const requestId = requestIdOf(req.requestId);
    const id = idOf(idPath, rawId);
    const change = prepare();
    const recorded = this.recorded(rpc, requestId, id);
    if (recorded) return { item: this.get(id), result: recorded.result };
    const item = this.get(id);
    if (req.expectedRevision !== undefined && req.expectedRevision !== item.revision) {
      throw errorInfo(
        Code.Aborted,
        "REVISION_MISMATCH",
        `item ${id}: revision conflict: expected ${req.expectedRevision}, found ${item.revision}`,
        { item_id: id, expected_revision: String(req.expectedRevision), actual_revision: String(item.revision) },
      );
    }
    if (item.deletedAt !== undefined && !ON_DELETED.has(rpc)) {
      throw errorInfo(Code.FailedPrecondition, "ITEM_DELETED", `item ${id} is deleted`, { item_id: id });
    }
    const patch = change(item);
    const next = patch === undefined ? item : this.update(id, patch);
    return { item: next, result: this.record(rpc, requestId, id) };
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

  /** A new op's result at the current revision, remembered for replays. */
  private record(rpc: string, requestId: string, id: string): MutationResult {
    const result = { opId: ulid(), revision: this.revision };
    this.requests.set(requestId, { rpc, id, result });
    return result;
  }

  private get(id: string): Item {
    const item = this.items.get(id);
    if (!item) throw errorInfo(Code.NotFound, "ITEM_NOT_FOUND", `item ${id} not found`, { item_id: id });
    return item;
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

function msg(message: MessageInitShape<typeof SubscribeResponseSchema>["message"]): SubscribeResponse {
  return create(SubscribeResponseSchema, { message });
}

/** An error with an ErrorInfo detail, as the daemon sends them. */
function errorInfo(code: Code, reason: string, message: string, metadata: Record<string, string>): ConnectError {
  return new ConnectError(message, code, undefined, [{ desc: ErrorInfoSchema, value: { reason, domain: "todo-tui", metadata } }]);
}

/** INVALID_ARGUMENT with one BadRequest violation, as the daemon sends them. */
function badRequest(field: string, description: string): ConnectError {
  return new ConnectError(`${field}: ${description}`, Code.InvalidArgument, undefined, [
    { desc: BadRequestSchema, value: { fieldViolations: [{ field, description }] } },
  ]);
}

const TOO_LONG = `is longer than ${MAX_CAPTURE_BYTES / 1024} KiB`;

/** Whether `text` is over MAX_CAPTURE_BYTES as UTF-8, as protobuf encodes it. */
function overCaptureLimit(text: string): boolean {
  // A UTF-16 code unit is at most 3 bytes of UTF-8.
  return text.length * 3 > MAX_CAPTURE_BYTES && new TextEncoder().encode(text).length > MAX_CAPTURE_BYTES;
}

const ULID = /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/i;

/** The request id in canonical (upper) case, so every spelling of it replays. */
function requestIdOf(requestId: string): string {
  if (!ULID.test(requestId)) throw badRequest("request_id", "must be a ULID");
  return requestId.toUpperCase();
}

/** The Item id at request field `path`, in canonical (upper) case. */
function idOf(path: string, id: string): string {
  if (!ULID.test(id)) throw badRequest(path, `invalid ULID ${JSON.stringify(id)}`);
  return id.toUpperCase();
}

/** `prev` with the fields `init` names replaced. */
function merged(prev: Item, init: MessageInitShape<typeof ItemSchema>): Item {
  const patch = create(ItemSchema, init) as unknown as Record<string, unknown>;
  const item = { ...prev } as unknown as Record<string, unknown>;
  for (const k of Object.keys(init)) item[k] = patch[k];
  return item as unknown as Item;
}

/** Item fields that change only through SetStatus. */
const STATUS_PATHS = new Set(["status", "status_category", "resolution_kind", "todo_refinement"]);

/**
 * The top-level Item fields named by the update mask, taken from the
 * request's item. Rejects the mask as the daemon does when it is missing,
 * empty or names an unknown field, a status field, archived or relations.
 */
function maskedFields(req: UpdateItemRequest): MessageInitShape<typeof ItemSchema> {
  if (req.updateMask === undefined) throw badRequest("update_mask", "required field is missing");
  if (req.updateMask.paths.length === 0) throw badRequest("update_mask", "must not be empty");
  const source = (req.item ?? create(ItemSchema)) as unknown as Record<string, unknown>;
  const patch: Record<string, unknown> = {};
  for (const path of req.updateMask.paths) {
    const top = path.split(".")[0]!;
    const field = ItemSchema.fields.find((f) => f.name === top);
    if (!field) throw badRequest("update_mask", `unknown field path ${JSON.stringify(path)}`);
    if (STATUS_PATHS.has(top)) throw badRequest(`item.${top}`, "changes only through SetStatus");
    if (top === "archived") throw badRequest("item.archived", "changes only through ArchiveItem and UnarchiveItem");
    if (top === "relations") throw badRequest("item.relations", "changes only through AddRelation and RemoveRelation");
    patch[field.localName] = source[field.localName];
  }
  return patch as MessageInitShape<typeof ItemSchema>;
}

/** A date as a sortable `YYYYMMDD` number. */
function dateKey(d: MessageInitShape<typeof DateSchema>): number {
  return (d.year ?? 0) * 10_000 + (d.month ?? 0) * 100 + (d.day ?? 0);
}

/** Today in the local time zone, as a dateKey. */
function localToday(): number {
  const now = new Date();
  return dateKey({ year: now.getFullYear(), month: now.getMonth() + 1, day: now.getDate() });
}
