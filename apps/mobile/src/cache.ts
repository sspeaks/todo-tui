// The list the phone last showed, kept to show at the next launch while
// the computer answers (LiveStoreOptions.cache): the tabs, the counts and
// the first tab's rows, the tab launch opens on. One server's at a time.
// The computer's answer always replaces it; unpairing forgets it.
import AsyncStorage from "@react-native-async-storage/async-storage";
import { type LiveStore, type StoreCache, decodeStoreCache, encodeStoreCache, viewKey } from "@todo-tui/client/portable";
import { tabQuery } from "@todo-tui/present";
import { useEffect, useRef } from "react";
import { AppState } from "react-native";

const KEY = "todo-tui.cache";

/** How long after a change the cache is written again: at most this often. */
export const WRITE_AFTER_MS = 5_000;

/** The kept cache and the server it is for; undefined when there is none (or it cannot be read). */
export async function loadCache(): Promise<{ url: string; cache: StoreCache } | undefined> {
  try {
    const raw = await AsyncStorage.getItem(KEY);
    if (!raw) return undefined;
    const { url, cache } = JSON.parse(raw) as { url?: unknown; cache?: unknown };
    const decoded = decodeStoreCache(cache);
    return typeof url === "string" && decoded ? { url, cache: decoded } : undefined;
  } catch {
    return undefined;
  }
}

export const saveCache = (url: string, cache: StoreCache): Promise<void> => AsyncStorage.setItem(KEY, JSON.stringify({ url, cache: encodeStoreCache(cache) }));

export const clearCache = (): Promise<void> => AsyncStorage.removeItem(KEY);

/**
 * What launch shows, of what the store shows (`now`): the tabs, the counts
 * and the first tab's rows. A first tab the store no longer holds (another
 * tab open past its grace) keeps its rows from `previous`, with the store's
 * own copy of any Item it still has.
 */
export function launchCache(now: StoreCache, previous?: StoreCache): StoreCache {
  const views = now.views.length > 0 ? now.views : (previous?.views ?? []);
  const counts = now.counts ?? previous?.counts;
  const rest = { views, ...(counts && { counts }) };
  const first = views[0];
  if (!first) return { ...rest, lists: [], items: [] };
  const key = viewKey(tabQuery(first.id));
  const from = now.lists.some((l) => l.key === key) ? now : previous;
  const list = from?.lists.find((l) => l.key === key);
  if (!from || !list) return { ...rest, lists: [], items: [] };
  const fresh = new Map(now.items.map((i) => [i.id, i]));
  const kept = new Map(from.items.map((i) => [i.id, i]));
  const items = list.ids.flatMap((id) => {
    const item = fresh.get(id) ?? kept.get(id);
    return item ? [item] : [];
  });
  return { ...rest, lists: [list], items };
}

/**
 * Keeps the launch cache of the store: once it has loaded from the
 * computer, again at most every WRITE_AFTER_MS while it changes, and at
 * once when the app goes to the background.
 */
export function CacheWriter({ store, url }: { store: LiveStore; url: string }) {
  /** The cache last written, or the one the store started from: the first tab's rows when the store has let them go. */
  const previous = useRef<StoreCache | undefined>(undefined);
  useEffect(() => {
    if (store.getConnection().cached) previous.current = store.toCache();
    const state = { written: -1, timer: undefined as ReturnType<typeof setTimeout> | undefined };
    const write = () => {
      clearTimeout(state.timer);
      state.timer = undefined;
      // Until the computer has answered, the store shows the cache (or nothing): nothing new to keep.
      if (store.getConnection().cached || store.getViews().status !== "ready" || state.written === store.getVersion()) return;
      state.written = store.getVersion();
      const cache = launchCache(store.toCache(), previous.current);
      previous.current = cache;
      saveCache(url, cache).catch(() => {});
    };
    const unsubscribe = store.subscribe(() => {
      state.timer ??= setTimeout(write, WRITE_AFTER_MS);
    });
    const sub = AppState.addEventListener("change", (s) => {
      if (s === "background") write();
    });
    return () => {
      unsubscribe();
      sub.remove();
      clearTimeout(state.timer);
    };
  }, [store, url]);
  return null;
}
