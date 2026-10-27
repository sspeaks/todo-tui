import type { ItemsClient, LiveStore, LiveStoreOptions } from "@todo-tui/client";
import { expect, type Mock, vi } from "vitest";
import { type FakeDaemonHandle, fakeDaemon, rows } from "../fake.test-util.js";
import { initialList } from "../state/selection.js";
import { Controller } from "./controller.js";
import type { Timing } from "./pipeline.js";

const running: { c: Controller; store: LiveStore }[] = [];

/** Stops every controller setup started, and its store; for afterEach. */
export async function stopControllers(): Promise<void> {
  for (const { c, store } of running.splice(0)) {
    c.stop();
    await store.stop();
  }
}

export interface SetupOptions {
  columns?: number;
  rows?: number;
  now?: () => Date;
  store?: Partial<LiveStoreOptions>;
  timing?: Partial<Timing>;
  /** The ItemService client actions call (default: the daemon's), e.g. one that holds or fails calls. */
  items?: (items: ItemsClient) => ItemsClient;
}

/** A started controller over FakeDaemon, 100×30 (a 27-row list), on the first tab with its rows loaded. */
export async function setup(fd: FakeDaemonHandle = fakeDaemon(rows(5)), opts: SetupOptions = {}) {
  const { columns = 100, rows: height = 30, now, timing } = opts;
  const store = fd.store(opts.store);
  const daemonItems = fd.clients().items;
  const items = opts.items ? opts.items(daemonItems) : daemonItems;
  const c = new Controller({ store, items, ...(now && { now }), ...(timing && { timing }) });
  running.push({ c, store });
  const quit: Mock<() => void> = vi.fn();
  c.onQuit = quit;
  c.setSize(columns, height);
  store.start();
  c.start();
  const state = () => c.ui.getState();
  await vi.waitFor(() => {
    const tab = state().tab;
    expect(tab && store.view({ viewId: tab }).getSnapshot().status).toBe("ready");
  });
  // The undo hint, as the status bar shows it.
  await vi.waitFor(() => expect(store.undoState.getSnapshot().status).toBe("ready"));
  const list = () => state().lists.get(state().tab ?? "") ?? initialList;
  const tabIs = (seed: string) => state().tab === fd.viewId(seed);
  const titles = () => store.view({ viewId: state().tab ?? "" }).getSnapshot().items.map((i) => i.title);
  /** The ops the user's own RPCs made (not the tests' put and update). */
  const userOps = () => fd.daemon.ops.filter((o) => o.actor === "user");
  /** An Item in the daemon by title. */
  const byTitle = (title: string) => [...fd.daemon.items.values()].find((i) => i.title === title);
  return {
    ...fd,
    c,
    store,
    quit,
    state,
    list,
    tabIs,
    titles,
    userOps,
    byTitle,
    mode: () => state().mode,
    toast: () => state().toast?.text,
    selected: () => c.store.getItem(c.selectedId() ?? "")?.title,
  };
}

export type Setup = Awaited<ReturnType<typeof setup>>;

/**
 * An ItemsClient whose calls named in `methods` wait for `release()`:
 * an action held in flight (hard case 36).
 */
export function holding(methods: readonly string[]) {
  let open: () => void = () => {};
  let gate = Promise.resolve();
  const held = { count: 0 };
  return {
    held,
    /** Holds the next calls until release(). */
    hold() {
      gate = new Promise<void>((resolve) => (open = resolve));
    },
    release: () => open(),
    wrap: (items: ItemsClient): ItemsClient =>
      new Proxy(items, {
        get(target, prop, receiver) {
          const value = Reflect.get(target, prop, receiver);
          if (typeof prop !== "string" || !methods.includes(prop) || typeof value !== "function") return value;
          return async (...args: unknown[]) => {
            held.count++;
            await gate;
            return (value as (...a: unknown[]) => unknown).apply(target, args);
          };
        },
      }),
  };
}
