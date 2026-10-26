import {
  FakeDaemon,
  type ItemSchema,
  type ListItemsRequest,
  LiveStore,
  type MessageInitShape,
  Priority,
  Status,
  timestampFromDate,
} from "@todo-tui/client";

type ItemInit = MessageInitShape<typeof ItemSchema> & { id: string };

let seq = 0;
/** A new item for FakeDaemon.put; each one is created a second after the previous one. */
export function item(title: string, init: { status?: Status; priority?: Priority; createdAt?: Date } = {}): ItemInit {
  seq += 1;
  return {
    id: `01J${String(seq).padStart(23, "0")}`,
    title,
    status: init.status ?? Status.INBOX,
    priority: init.priority ?? Priority.NONE,
    createdAt: timestampFromDate(init.createdAt ?? new Date(Date.UTC(2026, 0, 1, 0, 0, seq))),
  };
}

/** A FakeDaemon (the client's, which answers ListItems as the daemon does) that records the ListItems requests it gets. */
export function fakeDaemon(initial: ItemInit[] = []) {
  const daemon = new FakeDaemon(initial);
  const requests: ListItemsRequest[] = [];
  daemon.onListItems = (req) => void requests.push(req);
  return {
    daemon,
    put: (it: ItemInit) => daemon.put(it),
    /** Every ListItems request received. */
    requests,
    get revision() {
      return daemon.revision;
    },
    /** Fails every open Subscribe stream, as a daemon restart would. */
    dropStreams: () => daemon.dropStreams(),
    store: () => new LiveStore({ transport: daemon.transport, debounceMs: 5, backoff: { initialMs: 20, maxMs: 100 } }),
  };
}
