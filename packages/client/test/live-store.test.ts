import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import { afterEach, describe, expect, it } from "vitest";
import { ItemSchema } from "../src/gen/todo/v1/item_pb.js";
import { FakeDaemon } from "../src/fake-daemon.js";
import { LiveStore, type LiveStoreOptions } from "../src/live-store.js";

async function waitFor(cond: () => boolean, timeoutMs = 2000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!cond()) {
    if (Date.now() > deadline) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 5));
  }
}
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

let stores: LiveStore[] = [];
function setup(initial = 2, opts: Partial<LiveStoreOptions> = {}) {
  const daemon = new FakeDaemon(Array.from({ length: initial }, (_, i) => ({ title: `item ${i}` })));
  const store = new LiveStore({
    transport: daemon.transport,
    debounceMs: 20,
    backoff: { initialMs: 10, maxMs: 40 },
    viewGraceMs: 10,
    ...opts,
  });
  stores.push(store);
  return { daemon, store };
}
const titles = (store: LiveStore) => store.view().getSnapshot().items.map((i) => i.title);

afterEach(async () => {
  await Promise.all(stores.map((s) => s.stop()));
  stores = [];
});

describe("LiveStore", () => {
  it("loads views, subscribes from the loaded revision and goes live", async () => {
    const { daemon, store } = setup(3);
    const view = store.view();
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    expect(view.getSnapshot().status).toBe("ready");
    expect(titles(store)).toEqual(["item 0", "item 1", "item 2"]);
    expect(daemon.calls.subscribe).toEqual([3n]);
    expect(store.getConnection().revision).toBe(3n);
  });

  it("applies item changes immediately and re-queries membership after the debounce", async () => {
    const { daemon, store } = setup(2);
    const view = store.view();
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    const [first] = view.getSnapshot().items;

    daemon.update(first!.id, { title: "renamed" });
    await waitFor(() => store.getItem(first!.id)?.title === "renamed");
    expect(view.getSnapshot().items[0]?.title).toBe("renamed");

    daemon.remove(first!.id);
    await waitFor(() => store.getItem(first!.id) === undefined);
    expect(view.getSnapshot().items.map((i) => i.title)).toEqual(["item 1"]);

    daemon.put({ title: "new" });
    await waitFor(() => titles(store).includes("new"));
    expect(store.getConnection().revision).toBe(daemon.revision);
  });

  it("debounces bursts of changes into one ListItems", async () => {
    const { daemon, store } = setup(1, { debounceMs: 60 });
    store.view();
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    const before = daemon.calls.listItems;
    for (let i = 0; i < 10; i++) daemon.put({ title: `burst ${i}` });
    await waitFor(() => store.getConnection().revision === daemon.revision);
    expect(daemon.calls.listItems).toBe(before);
    await waitFor(() => titles(store).length === 11);
    await sleep(100);
    expect(daemon.calls.listItems).toBe(before + 1);
  });

  it("resumes from the last seen revision after a disconnect", async () => {
    const { daemon, store } = setup(2);
    store.view();
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    daemon.put({ title: "a" });
    await waitFor(() => store.getConnection().revision === 3n);

    daemon.down = true;
    daemon.dropStreams();
    await waitFor(() => store.getConnection().state === "reconnecting");
    daemon.put({ title: "while down" });
    await sleep(30);
    daemon.down = false;

    await waitFor(() => store.getConnection().state === "live" && titles(store).includes("while down"));
    expect(daemon.calls.subscribe.at(-1)).toBe(3n);
    expect(store.getConnection().revision).toBe(4n);
  });

  it("retries with backoff and asks for the daemon once per outage", async () => {
    let spawned = 0;
    const { daemon, store } = setup(1, {
      ensureDaemon: async () => {
        spawned++;
      },
    });
    daemon.down = true;
    store.start();
    await waitFor(() => store.getConnection().attempt >= 4);
    expect(spawned).toBe(1);
    daemon.down = false;
    await waitFor(() => store.getConnection().state === "live");
    expect(store.getConnection().attempt).toBe(0);
  });

  it("reloads everything on ResyncRequired", async () => {
    const { daemon, store } = setup(2);
    const view = store.view();
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    const [gone] = view.getSnapshot().items;

    daemon.down = true;
    daemon.dropStreams();
    await waitFor(() => store.getConnection().state === "reconnecting");
    // Changes made while the store is away fall out of the replay window.
    daemon.items.delete(gone!.id);
    daemon.put({ title: "added" });
    daemon.truncateChanges();
    daemon.down = false;

    await waitFor(() => titles(store).includes("added"));
    expect(titles(store)).toEqual(["item 1", "added"]);
    expect(store.getItem(gone!.id)).toBeUndefined();
    expect(store.getConnection().revision).toBe(daemon.revision);
  });

  it("resyncs onto a restored database whose revision went backwards", async () => {
    const { daemon, store } = setup(2);
    const view = store.view();
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    const [x, y] = view.getSnapshot().items;
    for (let i = 0; i < 8; i++) daemon.update(x!.id, { title: `edit ${i}` });
    await waitFor(() => store.getItem(x!.id)?.title === "edit 7");
    expect(store.getConnection().revision).toBe(10n);

    daemon.down = true;
    daemon.dropStreams();
    await waitFor(() => store.getConnection().state === "reconnecting");
    // Same ids, older content, lower revision.
    daemon.restore(3n, [
      create(ItemSchema, { ...x!, title: "restored", revision: 3n }),
      create(ItemSchema, { ...y!, revision: 2n }),
    ]);
    daemon.down = false;

    await waitFor(() => store.getItem(x!.id)?.title === "restored");
    expect(titles(store)).toEqual(["restored", "item 1"]);
    expect(store.getConnection().revision).toBe(3n);

    // Changes on the new timeline apply although their revisions are below
    // what the old timeline reached.
    daemon.update(x!.id, { title: "after restore" });
    await waitFor(() => store.getItem(x!.id)?.title === "after restore");
    daemon.put({ title: "new" });
    await waitFor(() => titles(store).includes("new"));
  });

  it("retries a view whose re-query failed once the stream comes back", async () => {
    const { daemon, store } = setup(1);
    const view = store.view();
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    let failed = 0;
    daemon.onListItems = () => {
      failed++;
      throw new ConnectError("daemon restarting", Code.Unavailable);
    };
    const added = daemon.put({ title: "added" });
    await waitFor(() => failed > 0);
    expect(store.getItem(added.id)?.title).toBe("added");
    expect(titles(store)).toEqual(["item 0"]);

    daemon.onListItems = undefined;
    daemon.dropStreams();
    await waitFor(() => titles(store).includes("added"));
    expect(view.getSnapshot().status).toBe("ready");
  });

  it("goes live when the daemon rejects one view's query", async () => {
    const { daemon, store } = setup(2);
    daemon.onListItems = (req) => {
      if (req.viewId === "unsupported") throw new ConnectError("not yet", Code.Unimplemented);
    };
    const good = store.view();
    const bad = store.view({ viewId: "unsupported" });
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    expect(good.getSnapshot().items).toHaveLength(2);
    expect(bad.getSnapshot().status).toBe("error");
    expect(daemon.calls.subscribe).toEqual([2n]);
    daemon.put({ title: "live" });
    await waitFor(() => titles(store).includes("live"));
  });

  it("subscribes with the ListItems revision when no view exists", async () => {
    const { daemon, store } = setup(4);
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    expect(daemon.calls.subscribe).toEqual([4n]);
    const view = store.view();
    await waitFor(() => view.getSnapshot().status === "ready");
    expect(view.getSnapshot().items).toHaveLength(4);
  });

  it("pages through ListItems", async () => {
    const { daemon, store } = setup(25);
    daemon.pageSize = 10;
    const view = store.view();
    store.start();
    await waitFor(() => view.getSnapshot().status === "ready");
    expect(view.getSnapshot().items).toHaveLength(25);
  });

  it("keeps snapshots referentially stable and shares equal views", async () => {
    const { daemon, store } = setup(2);
    const view = store.view({ sort: [{ key: 10 }] });
    expect(store.view({ sort: [{ key: 10 }] })).toBe(view);
    expect(store.view()).not.toBe(view);
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    const a = view.getSnapshot();
    expect(view.getSnapshot()).toBe(a);
    daemon.put({ title: "x" }); // changes the store version, not this view until re-query
    await waitFor(() => store.getConnection().revision === daemon.revision);
    expect(view.getSnapshot()).toBe(a);
    await waitFor(() => view.getSnapshot() !== a);
    expect(view.getSnapshot().items).toHaveLength(3);
  });

  it("drops unobserved views after the grace period", async () => {
    const { store } = setup(1);
    const view = store.view();
    const unsub = view.subscribe(() => {});
    unsub();
    await sleep(30);
    expect(store.view()).not.toBe(view);
  });

  it("delivers notices", async () => {
    const { store } = setup(0);
    const seen: string[] = [];
    store.onNotice((n) => {
      if (n.kind.case === "toast") seen.push(n.kind.value.message);
    });
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    expect(seen).toEqual([]);
  });
});
