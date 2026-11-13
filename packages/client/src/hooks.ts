import { createContext, createElement, type ReactNode, useContext, useEffect, useMemo, useSyncExternalStore } from "react";
import type { TodoClients } from "./clients.js";
import type { Item } from "./gen/todo/v1/item_pb.js";
import type { SyncStatus } from "./gen/todo/v1/sync_service_pb.js";
import type {
  ConnectionSnapshot,
  CountsSnapshot,
  LiveConfigSnapshot,
  LiveStore,
  PeopleSnapshot,
  SavedViewsSnapshot,
  SuggestionsSnapshot,
  UndoSnapshot,
  ViewQuery,
  ViewSnapshot,
} from "./live-store.js";

const LiveStoreContext = createContext<LiveStore | null>(null);
const ClientsContext = createContext<TodoClients | null>(null);

export function LiveStoreProvider({ store, children }: { store: LiveStore; children?: ReactNode }) {
  return createElement(LiveStoreContext.Provider, { value: store }, children);
}

export function useLiveStore(): LiveStore {
  const store = useContext(LiveStoreContext);
  if (!store) throw new Error("useLiveStore: no <LiveStoreProvider> above this component");
  return store;
}

/** Gives components the RPC clients: a TodoConnection from connect(), or createClients(transport) in tests. */
export function ClientsProvider({ clients, children }: { clients: TodoClients; children?: ReactNode }) {
  return createElement(ClientsContext.Provider, { value: clients }, children);
}

export function useClients(): TodoClients {
  const clients = useContext(ClientsContext);
  if (!clients) throw new Error("useClients: no <ClientsProvider> above this component");
  return clients;
}

/**
 * The live, server-ordered result of a ListItems query. Equal queries share
 * one view, so `query` need not be memoised.
 */
export function useView(query: ViewQuery = {}): ViewSnapshot {
  const store = useLiveStore();
  const view = store.view(query);
  return useSyncExternalStore(view.subscribe, view.getSnapshot);
}

/**
 * An Item from the store. With `fetch`, an Item the store does not hold (a
 * parent, a relation's other end) is fetched with GetItem while the
 * connection is live, and again after a failed fetch once it is live again;
 * an id the daemon does not know stays undefined.
 */
export function useItem(id: string | undefined, opts: { fetch?: boolean } = {}): Item | undefined {
  const store = useLiveStore();
  const getSnapshot = useMemo(() => () => (id === undefined ? undefined : store.getItem(id)), [store, id]);
  const item = useSyncExternalStore(store.subscribe, getSnapshot);
  const live = useSyncExternalStore(store.subscribe, () => store.getConnection().state === "live");
  const fetch = opts.fetch === true && id !== undefined && item === undefined && live;
  useEffect(() => {
    if (fetch && id !== undefined) store.fetchItem(id).catch(() => {});
  }, [store, id, fetch]);
  return item;
}

export function useConnection(): ConnectionSnapshot {
  const store = useLiveStore();
  return useSyncExternalStore(store.subscribe, store.getConnection);
}

/** The saved views (the tabs) in tab order, kept live from TOPIC_VIEW changes. */
export function useViews(): SavedViewsSnapshot {
  const store = useLiveStore();
  return useSyncExternalStore(store.subscribe, store.getViews);
}

/** The daemon's config (GetConfig), kept live from TOPIC_CONFIG changes. */
export function useConfig(): LiveConfigSnapshot {
  const store = useLiveStore();
  return useSyncExternalStore(store.subscribe, store.getConfig);
}

/** Each configured instance's sync state, kept live from Notice.sync_status. */
export function useSyncStatus(): ReadonlyMap<string, SyncStatus> {
  const store = useLiveStore();
  return useSyncExternalStore(store.subscribe, store.getSyncStatus);
}

/**
 * The status and tab counts (GetStatusCounts with include_views), re-read
 * (debounced) after Item and view changes while a component observes them.
 */
export function useStatusCounts(): CountsSnapshot {
  const store = useLiveStore();
  return useSyncExternalStore(store.subscribeCounts, store.getCounts);
}

/**
 * The undo and redo entries (GetUndoState), re-read (debounced) after Item
 * changes while a component observes them, and at once on
 * `store.undoState.reload()`.
 */
export function useUndoState(): UndoSnapshot {
  const store = useLiveStore();
  return useSyncExternalStore(store.undoState.subscribe, store.undoState.getSnapshot);
}

/**
 * The Suggestions that wait (ListSuggestions), kept live from
 * TOPIC_SUGGESTION changes while a component observes them.
 */
export function useSuggestions(): SuggestionsSnapshot {
  const store = useLiveStore();
  return useSyncExternalStore(store.suggestions.subscribe, store.suggestions.getSnapshot);
}

/** The People the daemon knows (design 3.5), by id, kept live from TOPIC_PERSON changes. */
export function usePeople(): PeopleSnapshot {
  const store = useLiveStore();
  return useSyncExternalStore(store.people.subscribe, store.people.getSnapshot);
}
