import { type MessageInitShape, create } from "@bufbuild/protobuf";
import { timestampNow } from "@bufbuild/protobuf/wkt";
import { Code, ConnectError, type Transport, createRouterTransport } from "@connectrpc/connect";
import { ErrorInfoSchema } from "./gen/google/rpc/error_details_pb.js";
import { type MutationResultSchema, Topic } from "./gen/todo/v1/common_pb.js";
import { ArchiveReason, type Item, ItemSchema, Status } from "./gen/todo/v1/item_pb.js";
import { ItemService, type ListItemsRequest, type UpdateItemRequest } from "./gen/todo/v1/item_service_pb.js";
import { SystemService } from "./gen/todo/v1/system_service_pb.js";
import { type Change, ChangeSchema, type SubscribeResponse, SubscribeResponseSchema, WatchService } from "./gen/todo/v1/watch_service_pb.js";

type Waiter = (msg: SubscribeResponse | Error) => void;

interface MutationResponse {
  item: Item;
  result: MessageInitShape<typeof MutationResultSchema>;
}

/**
 * An in-memory daemon over createRouterTransport for tests: ItemService,
 * WatchService with replay / CaughtUp / ResyncRequired, and
 * SystemService.GetServerInfo. ListItems only filters out soft-deleted items
 * (unless include_deleted) and sorts by id. The single-item mutations honour
 * request_id replay and expected_revision but none of the domain rules:
 * SetStatus sets only the derived status, UpdateItem copies the top-level
 * fields its mask names.
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
  apiMinor = 1;
  readonly calls = { listItems: 0, subscribe: [] as (bigint | undefined)[] };
  readonly transport: Transport;
  private readonly watchers = new Set<Waiter>();
  /** Mutation responses by request_id, for replays. */
  private readonly responses = new Map<string, MutationResponse>();
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
          const item = this.put({ title: req.text, createdVia: req.createdVia });
          return { item, result: { opId: `op${this.revision}`, revision: this.revision } };
        },
        listItems: (req) => {
          this.check();
          this.calls.listItems++;
          this.onListItems?.(req);
          const all = [...this.items.values()]
            .filter((i) => req.filter?.includeDeleted || i.deletedAt === undefined)
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
          return { item: this.get(req.id), revision: this.revision };
        },
        updateItem: (req) => this.mutate({ ...req, id: req.item?.id ?? "" }, maskedFields(req)),
        setStatus: (req) => this.mutate(req, { status: req.status }),
        archiveItem: (req) => this.mutate(req, { archived: { at: timestampNow(), reason: ArchiveReason.USER } }),
        unarchiveItem: (req) => this.mutate(req, { archived: undefined }),
        deleteItem: (req) => this.mutate(req, { deletedAt: timestampNow() }),
        undeleteItem: (req) => this.mutate(req, { deletedAt: undefined }),
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
    let item: Item;
    if (prev) {
      const patch = create(ItemSchema, init) as unknown as Record<string, unknown>;
      item = { ...prev };
      for (const k of Object.keys(init)) (item as unknown as Record<string, unknown>)[k] = patch[k];
    } else {
      item = create(ItemSchema, { status: Status.INBOX, createdAt: timestampNow(), ...init });
    }
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
   * Applies `patch` as a mutation: a replayed request_id returns the first
   * response, and a stale expected_revision fails with ABORTED.
   */
  private mutate(
    req: { requestId: string; id: string; expectedRevision?: bigint | undefined },
    patch: MessageInitShape<typeof ItemSchema>,
  ): MutationResponse {
    this.check();
    const replay = this.responses.get(req.requestId);
    if (replay) return replay;
    const actual = this.get(req.id).revision;
    if (req.expectedRevision !== undefined && req.expectedRevision !== actual) {
      throw errorInfo(Code.Aborted, "REVISION_MISMATCH", `item ${req.id} is at revision ${actual}, not ${req.expectedRevision}`, {
        expected_revision: String(req.expectedRevision),
        actual_revision: String(actual),
      });
    }
    const res = { item: this.update(req.id, patch), result: { opId: `op${this.revision}`, revision: this.revision } };
    if (req.requestId !== "") this.responses.set(req.requestId, res);
    return res;
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

/** The top-level Item fields named by the update mask, taken from the request's item. */
function maskedFields(req: UpdateItemRequest): MessageInitShape<typeof ItemSchema> {
  const source = (req.item ?? create(ItemSchema)) as unknown as Record<string, unknown>;
  const patch: Record<string, unknown> = {};
  for (const path of req.updateMask?.paths ?? []) {
    const field = ItemSchema.fields.find((f) => f.name === path.split(".")[0]);
    if (!field) throw new ConnectError(`update_mask: unknown field ${path}`, Code.InvalidArgument);
    patch[field.localName] = source[field.localName];
  }
  return patch as MessageInitShape<typeof ItemSchema>;
}
