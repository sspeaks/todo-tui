import {
  Code,
  ConnectError,
  ItemField,
  ItemSchema,
  ItemService,
  ListItemsResponseSchema,
  LiveStore,
  Operator,
  Priority,
  Status,
  SubscribeResponseSchema,
  Topic,
  WatchService,
  create,
  createRouterTransport,
  timestampFromDate,
  type Item,
  type ListItemsRequest,
  type SubscribeResponse,
} from "@todo-tui/client";

let seq = 0;
/** A new item; each one is created a second after the previous one. */
export function item(title: string, init: { status?: Status; priority?: Priority; createdAt?: Date } = {}): Item {
  seq += 1;
  return create(ItemSchema, {
    id: `01J${String(seq).padStart(23, "0")}`,
    title,
    status: init.status ?? Status.INBOX,
    effectivePriority: init.priority ?? Priority.NONE,
    createdAt: timestampFromDate(init.createdAt ?? new Date(Date.UTC(2026, 0, 1, 0, 0, seq))),
  });
}

const unimplemented = (what: string) => new ConnectError(`ListItems: ${what} is not supported yet`, Code.Unimplemented);

/**
 * ListItems as the P0 daemon answers it (service/item.rs): at most one
 * `status IN/EQ` predicate in all_of, the implicit view rules, and only the
 * `created_at, id` order; anything else is UNIMPLEMENTED.
 */
function listItems(all: Iterable<Item>, req: ListItemsRequest): Item[] {
  const filter = req.filter;
  let statuses: Status[] = [];
  if (filter) {
    if (filter.anyOf.length > 0 || filter.allOf.length > 1) throw unimplemented("this filter");
    const clause = filter.allOf[0];
    if (clause) {
      const p = clause.kind.case === "predicate" ? clause.kind.value : undefined;
      if (!p || p.target !== ItemField.STATUS || ![Operator.IN, Operator.EQ].includes(p.op) || p.operands.length === 0) {
        throw unimplemented("this filter");
      }
      statuses = p.operands.map((o) => {
        const v = o.kind.case === "value" ? o.kind.value.kind : undefined;
        if (v?.case !== "status") throw new ConnectError("expected status values", Code.InvalidArgument);
        return v.value;
      });
    }
  }
  const order = [ItemField.CREATED_AT, ItemField.ID];
  if (req.sort.length > order.length || req.sort.some((k, i) => k.key !== order[i] || k.descending)) {
    throw unimplemented("this sort");
  }
  if (req.viewId !== undefined) throw unimplemented("view_id");
  const ms = (i: Item) => Number(i.createdAt?.seconds ?? 0n) * 1000 + (i.createdAt?.nanos ?? 0) / 1e6;
  return [...all]
    .filter((i) => statuses.length === 0 || statuses.includes(i.status))
    .filter((i) => filter?.includeDeleted || i.deletedAt === undefined)
    .filter((i) => filter?.includeArchived || i.archived === undefined)
    .filter((i) => filter?.includeDone || (i.status !== Status.DONE && i.status !== Status.CANCELLED))
    .sort((a, b) => ms(a) - ms(b) || a.id.localeCompare(b.id));
}

type Event = SubscribeResponse | Error;

/** In-memory ItemService + WatchService over createRouterTransport. */
export function fakeDaemon(initial: Item[] = []) {
  let revision = 0n;
  const requests: ListItemsRequest[] = [];
  const items = new Map<string, Item>();
  const log: SubscribeResponse[] = [];
  const watchers = new Set<(e: Event) => void>();

  const put = (it: Item) => {
    revision += 1n;
    const stored = { ...it, revision };
    items.set(it.id, stored);
    const msg = create(SubscribeResponseSchema, {
      message: {
        case: "change",
        value: { revision, topic: Topic.ITEM, id: it.id, resource: { case: "item", value: stored } },
      },
    });
    log.push(msg);
    for (const w of watchers) w(msg);
  };
  initial.forEach(put);

  const transport = createRouterTransport(({ service }) => {
    service(ItemService, {
      captureItem: () => {
        throw new Error("unused");
      },
      listItems: (req) => {
        requests.push(req);
        return create(ListItemsResponseSchema, { items: listItems(items.values(), req), revision });
      },
    });
    service(WatchService, {
      async *subscribe(req, ctx) {
        const queue: Event[] = [];
        let wake = () => {};
        const watcher = (e: Event) => (queue.push(e), wake());
        watchers.add(watcher);
        try {
          const since = req.sinceRevision ?? revision;
          for (const msg of log) if (msg.message.case === "change" && msg.message.value.revision > since) yield msg;
          yield create(SubscribeResponseSchema, { message: { case: "caughtUp", value: { revision } } });
          while (!ctx.signal.aborted) {
            const next = queue.shift();
            if (next instanceof Error) throw next;
            if (next) yield next;
            else await new Promise<void>((r) => ((wake = r), ctx.signal.addEventListener("abort", () => r())));
          }
        } finally {
          watchers.delete(watcher);
        }
      },
    });
  });

  return {
    put,
    /** Every ListItems request received. */
    requests,
    get revision() {
      return revision;
    },
    /** Fails every open Subscribe stream, as a daemon restart would. */
    dropStreams: () => watchers.forEach((w) => w(new Error("daemon went away"))),
    store: () => new LiveStore({ transport, debounceMs: 5, backoff: { initialMs: 20, maxMs: 100 } }),
  };
}
