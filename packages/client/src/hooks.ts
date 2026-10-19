import { createContext, createElement, type ReactNode, useContext, useMemo, useSyncExternalStore } from "react";
import type { Item } from "./gen/todo/v1/item_pb.js";
import type { ConnectionSnapshot, LiveStore, ViewQuery, ViewSnapshot } from "./live-store.js";

const LiveStoreContext = createContext<LiveStore | null>(null);

export function LiveStoreProvider({ store, children }: { store: LiveStore; children?: ReactNode }) {
  return createElement(LiveStoreContext.Provider, { value: store }, children);
}

export function useLiveStore(): LiveStore {
  const store = useContext(LiveStoreContext);
  if (!store) throw new Error("useLiveStore: no <LiveStoreProvider> above this component");
  return store;
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

export function useItem(id: string | undefined): Item | undefined {
  const store = useLiveStore();
  const getSnapshot = useMemo(() => () => (id === undefined ? undefined : store.getItem(id)), [store, id]);
  return useSyncExternalStore(store.subscribe, getSnapshot);
}

export function useConnection(): ConnectionSnapshot {
  const store = useLiveStore();
  return useSyncExternalStore(store.subscribe, store.getConnection);
}
