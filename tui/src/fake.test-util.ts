import {
  FakeDaemon,
  type FakeDaemonOptions,
  type ItemSchema,
  type ListItemsRequest,
  LiveStore,
  type LiveStoreOptions,
  type MessageInitShape,
  Priority,
  Status,
  createClients,
  timestampFromDate,
} from "@todo-tui/client";

type ItemInit = MessageInitShape<typeof ItemSchema> & { id: string };

let seq = 0;
/** A new item for FakeDaemon.put; each one is created a second after the previous one. */
export function item(
  title: string,
  init: Omit<MessageInitShape<typeof ItemSchema>, "id" | "title"> & { createdAt?: Date } = {},
): ItemInit {
  seq += 1;
  const { createdAt, ...rest } = init;
  return {
    id: `01J${String(seq).padStart(23, "0")}`,
    title,
    status: Status.INBOX,
    priority: Priority.NONE,
    ...rest,
    createdAt: timestampFromDate(createdAt ?? new Date(Date.UTC(2026, 0, 1, 0, 0, seq))),
  } as ItemInit;
}

/** `n` inbox items titled "row 0" … */
export const rows = (n: number): ItemInit[] => Array.from({ length: n }, (_, i) => item(`row ${i}`));

/**
 * A FakeDaemon (the client's, which answers ListItems as the daemon does)
 * that records the ListItems requests it gets, with its clock and zone.
 */
export function fakeDaemon(initial: ItemInit[] = [], options: FakeDaemonOptions = {}) {
  const daemon = new FakeDaemon(initial, options);
  const requests: ListItemsRequest[] = [];
  daemon.onListItems = (req) => void requests.push(req);
  return {
    daemon,
    put: (it: MessageInitShape<typeof ItemSchema>) => daemon.put(it),
    /** Every ListItems request received. */
    requests,
    get revision() {
      return daemon.revision;
    },
    /** Fails every open Subscribe stream, as a daemon restart would. */
    dropStreams: () => daemon.dropStreams(),
    /** A saved view's id by its seed ("inbox", "today", …). */
    viewId: (seed: string) => [...daemon.views.values()].find((v) => v.seed === seed)?.id ?? "",
    clients: () => createClients(daemon.transport),
    store: (opts: Partial<LiveStoreOptions> = {}) =>
      new LiveStore({ transport: daemon.transport, debounceMs: 5, backoff: { initialMs: 20, maxMs: 100 }, ...opts }),
  };
}

export type FakeDaemonHandle = ReturnType<typeof fakeDaemon>;
