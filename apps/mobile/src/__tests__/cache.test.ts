// The list kept for the next launch: the tabs, the counts and the first
// tab's rows, carried over while another tab is open; one computer's at a
// time, and anything unreadable read as none.
import AsyncStorage from "@react-native-async-storage/async-storage";
import { FakeDaemon } from "@todo-tui/client/fake";
import { ItemSchema, LiveStore, type StoreCache, ViewSchema, create, viewKey } from "@todo-tui/client/portable";
import { tabQuery } from "@todo-tui/present";
import { act, render } from "@testing-library/react-native";
import { createElement } from "react";
import { AppState, type AppStateStatus } from "react-native";
import { CacheWriter, launchCache, loadCache, saveCache } from "../cache";

const view = (id: string, position: number) => create(ViewSchema, { id, name: id, position });
const item = (id: string, title = id) => create(ItemSchema, { id, title });
const list = (tab: string, ids: string[]) => ({ key: viewKey(tabQuery(tab)), ids });

const tabs = [view("INBOX", 0), view("NEXT", 1)];
const counts = { views: new Map([["INBOX", 2]]) };

describe("launchCache", () => {
  it("keeps the tabs, the counts and the first tab's rows only", () => {
    const now: StoreCache = { views: tabs, lists: [list("INBOX", ["a", "b"]), list("NEXT", ["c"])], items: [item("a"), item("b"), item("c")], counts };
    expect(launchCache(now)).toEqual({ views: tabs, lists: [list("INBOX", ["a", "b"])], items: [item("a"), item("b")], counts });
  });

  it("carries the first tab's rows over while the store does not hold them, with the store's copy of an Item it still has", () => {
    const previous: StoreCache = { views: tabs, lists: [list("INBOX", ["a", "b"])], items: [item("a"), item("b")], counts };
    const now: StoreCache = { views: tabs, lists: [list("NEXT", ["b"])], items: [item("b", "b, renamed")], counts };
    expect(launchCache(now, previous)).toEqual({ views: tabs, lists: [list("INBOX", ["a", "b"])], items: [item("a"), item("b", "b, renamed")], counts });
  });

  it("keeps no rows without tabs", () => {
    expect(launchCache({ views: [], lists: [list("INBOX", ["a"])], items: [item("a")] })).toEqual({ views: [], lists: [], items: [] });
  });
});

describe("the kept cache", () => {
  beforeEach(() => AsyncStorage.clear());

  it("reads back what was saved, for the computer it was saved for", async () => {
    const cache: StoreCache = { views: tabs, lists: [list("INBOX", ["a"])], items: [item("a")], counts };
    await saveCache("https://mac.ts.net", cache);
    const kept = await loadCache();
    expect(kept?.url).toBe("https://mac.ts.net");
    expect(kept?.cache.items.map((i) => i.title)).toEqual(["a"]);
    expect(kept?.cache.lists).toEqual(cache.lists);
  });

  it("reads nothing, or anything unreadable, as none", async () => {
    expect(await loadCache()).toBeUndefined();
    await AsyncStorage.setItem("todo-tui.cache", "{not json");
    expect(await loadCache()).toBeUndefined();
    await AsyncStorage.setItem("todo-tui.cache", JSON.stringify({ url: "https://mac.ts.net", cache: { v: 99 } }));
    expect(await loadCache()).toBeUndefined();
  });
});

describe("CacheWriter", () => {
  const stores: LiveStore[] = [];
  let listeners: ((s: AppStateStatus) => void)[] = [];
  beforeEach(async () => {
    await AsyncStorage.clear();
    listeners = [];
    jest.spyOn(AppState, "addEventListener").mockImplementation((_type, listener) => {
      listeners.push(listener as (s: AppStateStatus) => void);
      return { remove: () => void (listeners = listeners.filter((l) => l !== listener)) };
    });
  });
  afterEach(async () => {
    jest.restoreAllMocks();
    await act(async () => {
      for (const s of stores.splice(0)) await s.stop();
    });
  });
  const background = () => act(async () => listeners.forEach((l) => l("background")));
  const until = async (cond: () => boolean) => {
    for (let i = 0; i < 200 && !cond(); i++) await act(() => new Promise((r) => setTimeout(r, 5)));
    expect(cond()).toBe(true);
  };

  it("keeps the first tab's rows once the store has loaded, when the app goes to the background", async () => {
    const daemon = new FakeDaemon([{ title: "buy milk" }]);
    const store = new LiveStore({ transport: daemon.transport, debounceMs: 0 });
    stores.push(store);
    const inbox = [...daemon.views.values()].sort((a, b) => a.position - b.position)[0]!.id;
    const unsubscribe = store.view(tabQuery(inbox)).subscribe(() => {});
    store.start();
    await render(createElement(CacheWriter, { store, url: "https://mac.ts.net" }));
    await until(() => store.getConnection().state === "live" && store.view(tabQuery(inbox)).getSnapshot().items.length === 1);

    await background();
    await until(() => true);
    const kept = await loadCache();
    expect(kept?.url).toBe("https://mac.ts.net");
    expect(kept?.cache.lists.map((l) => l.key)).toEqual([viewKey(tabQuery(inbox))]);
    expect(kept?.cache.items.map((i) => i.title)).toEqual(["buy milk"]);
    unsubscribe();
  });

  it("keeps nothing new while the store still shows the cache it started from", async () => {
    const daemon = new FakeDaemon([{ title: "buy milk" }]);
    daemon.down = true;
    const cache: StoreCache = { views: tabs, lists: [list("INBOX", ["a"])], items: [item("a")], counts };
    const store = new LiveStore({ transport: daemon.transport, cache, backoff: { initialMs: 10, maxMs: 20 } });
    stores.push(store);
    store.start();
    await render(createElement(CacheWriter, { store, url: "https://mac.ts.net" }));
    await background();
    expect(await AsyncStorage.getItem("todo-tui.cache")).toBeNull();
  });
});
