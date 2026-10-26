import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type Transport, createClient } from "@connectrpc/connect";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ItemField } from "../src/gen/todo/v1/field_pb.js";
import { Operator } from "../src/gen/todo/v1/filter_pb.js";
import { ItemSchema, Priority, Status } from "../src/gen/todo/v1/item_pb.js";
import { ItemService } from "../src/gen/todo/v1/item_service_pb.js";
import type { SubscribeResponse } from "../src/gen/todo/v1/watch_service_pb.js";
import { FakeDaemon } from "../src/fake-daemon.js";
import { MAX_PAGE_SIZE } from "../src/limits.js";
import { LiveStore, type LiveStoreOptions } from "../src/live-store.js";
import { RelationKind } from "../src/gen/todo/v1/relation_pb.js";
import { addRelation, deleteItem, removeRelation, undeleteItem, undo } from "../src/mutations.js";

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
    expect(daemon.calls.subscribe).toEqual([daemon.revision]);
    expect(store.getConnection().revision).toBe(daemon.revision);
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

  it("hides a soft-deleted item from views at once but keeps it in getItem", async () => {
    // No re-query within the test: what the views show comes from the store.
    const { daemon, store } = setup(2, { debounceMs: 60_000 });
    const items = createClient(ItemService, daemon.transport);
    const view = store.view();
    const withDeleted = store.view({ filter: { includeDeleted: true } });
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    const [a, b] = view.getSnapshot().items;

    await deleteItem(items, a!.id);
    await waitFor(() => store.getItem(a!.id)?.deletedAt !== undefined);
    const snap = view.getSnapshot();
    expect(snap.items).toEqual([b]);
    expect(withDeleted.getSnapshot().items.map((i) => i.id)).toEqual([a!.id, b!.id]);
    expect(store.getItem(a!.id)?.title).toBe("item 0");

    // Snapshots stay stable while the view's visible contents do not change.
    daemon.put({ title: "elsewhere" });
    await waitFor(() => store.getConnection().revision === daemon.revision);
    expect(view.getSnapshot()).toBe(snap);

    await undeleteItem(items, a!.id);
    await waitFor(() => view.getSnapshot().items.length === 2);
    expect(view.getSnapshot().items.map((i) => i.id)).toEqual([a!.id, b!.id]);
  });

  it("keeps Items' relations from changes and re-queries", async () => {
    const { daemon, store } = setup(2);
    const items = createClient(ItemService, daemon.transport);
    const view = store.view();
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    const [a, b] = view.getSnapshot().items;
    const relation = { fromId: a!.id, toId: b!.id, kind: RelationKind.BLOCKS };
    const keys = (id: string) => store.getItem(id)?.relations.map((r) => [r.fromId, r.toId, r.kind]);

    const calls = daemon.calls.listItems;
    await addRelation(items, relation);
    // Both ends change, each with its relations.
    await waitFor(() => keys(a!.id)?.length === 1 && keys(b!.id)?.length === 1);
    expect(keys(b!.id)).toEqual([[a!.id, b!.id, RelationKind.BLOCKS]]);
    expect(view.getSnapshot().items.map((i) => i.relations.length)).toEqual([1, 1]);
    // The re-query the changes trigger returns them too.
    await waitFor(() => daemon.calls.listItems > calls);
    await sleep(40);
    expect(view.getSnapshot().items.map((i) => i.relations.length)).toEqual([1, 1]);

    // A deleted end keeps its relation, on both Items.
    await deleteItem(items, b!.id);
    await waitFor(() => store.getItem(b!.id)?.deletedAt !== undefined);
    expect([keys(a!.id), keys(b!.id)]).toEqual([[[a!.id, b!.id, RelationKind.BLOCKS]], [[a!.id, b!.id, RelationKind.BLOCKS]]]);
    await undo(items);
    await waitFor(() => view.getSnapshot().items.length === 2);

    await removeRelation(items, relation);
    await waitFor(() => keys(a!.id)?.length === 0 && keys(b!.id)?.length === 0);
    await undo(items);
    await waitFor(() => keys(a!.id)?.length === 1 && keys(b!.id)?.length === 1);
  });

  it("re-shows an undeleted item after the re-query dropped it", async () => {
    const { daemon, store } = setup(2);
    const items = createClient(ItemService, daemon.transport);
    const view = store.view();
    const withDeleted = store.view({ filter: { includeDeleted: true } });
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    const [a] = view.getSnapshot().items;

    const calls = daemon.calls.listItems;
    await deleteItem(items, a!.id);
    await waitFor(() => daemon.calls.listItems >= calls + 2);
    await waitFor(() => !view.ids.includes(a!.id));
    expect(titles(store)).toEqual(["item 1"]);
    expect(withDeleted.getSnapshot().items.map((i) => i.title)).toEqual(["item 0", "item 1"]);
    expect(store.getItem(a!.id)?.deletedAt).toBeDefined();

    await undeleteItem(items, a!.id);
    await waitFor(() => titles(store).includes("item 0"));
    expect(titles(store)).toEqual(["item 0", "item 1"]);
    expect(store.getItem(a!.id)?.deletedAt).toBeUndefined();
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
    const seen = daemon.revision;
    await waitFor(() => store.getConnection().revision === seen);

    daemon.down = true;
    daemon.dropStreams();
    await waitFor(() => store.getConnection().state === "reconnecting");
    daemon.put({ title: "while down" });
    await sleep(30);
    daemon.down = false;

    await waitFor(() => store.getConnection().state === "live" && titles(store).includes("while down"));
    expect(daemon.calls.subscribe.at(-1)).toBe(seen);
    expect(store.getConnection().revision).toBe(seen + 1n);
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
    expect(store.getConnection().revision).toBe(daemon.revision);

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
    expect(daemon.calls.subscribe).toEqual([daemon.revision]);
    daemon.put({ title: "live" });
    await waitFor(() => titles(store).includes("live"));
  });

  it("subscribes with the ListViews revision when no view exists", async () => {
    const { daemon, store } = setup(4);
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    expect(daemon.calls.subscribe).toEqual([daemon.revision]);
    expect(daemon.calls.listItems).toBe(0);
    const view = store.view();
    await waitFor(() => view.getSnapshot().status === "ready");
    expect(view.getSnapshot().items).toHaveLength(4);
  });

  it("pages through ListItems", async () => {
    const { daemon, store } = setup(25);
    daemon.maxPageSize = 10;
    const view = store.view();
    store.start();
    await waitFor(() => view.getSnapshot().status === "ready");
    expect(view.getSnapshot().items).toHaveLength(25);
    expect(daemon.calls.listItems).toBe(3);
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

describe("LiveStore saved views, counts and paging", () => {
  const saved = (daemon: FakeDaemon, seed: string) => [...daemon.views.values()].find((v) => v.seed === seed)!;
  const statusIn = (status: Status) => ({
    allOf: [
      {
        kind: {
          case: "predicate" as const,
          value: { target: ItemField.STATUS, op: Operator.IN, operands: [{ kind: { case: "value" as const, value: { kind: { case: "status" as const, value: status } } } }] },
        },
      },
    ],
  });

  it("keeps the saved views in tab order from ListViews and view changes", async () => {
    const { daemon, store } = setup(0);
    expect(store.getViews()).toMatchObject({ status: "loading", views: [] });
    store.start();
    await waitFor(() => store.getViews().status === "ready");
    const snap = store.getViews();
    expect(snap.views.map((v) => v.seed)).toEqual(["inbox", "today", "next", "waiting", "someday", "archive"]);
    expect(store.getViews()).toBe(snap);
    expect(daemon.calls.listViews).toBe(1);

    const inbox = saved(daemon, "inbox");
    daemon.updateView(inbox.id, { name: "In", position: 9 });
    await waitFor(() => store.getViews().views.at(-1)?.name === "In");
    expect(store.getSavedView(inbox.id.toLowerCase())?.name).toBe("In");
    daemon.removeView(saved(daemon, "someday").id);
    await waitFor(() => store.getViews().views.length === 5);
    expect(store.getViews().views.map((v) => v.seed)).toEqual(["today", "next", "waiting", "archive", "inbox"]);
    expect(store.getConnection().revision).toBe(daemon.revision);
  });

  it("re-queries the views that narrow a saved view when it changes, and only those", async () => {
    const { daemon, store } = setup(0);
    daemon.put({ title: "inboxed" });
    daemon.put({ title: "next", status: Status.NEXT });
    const next = saved(daemon, "next");
    const narrowed = store.view({ viewId: next.id });
    const all = store.view();
    store.start();
    await waitFor(() => narrowed.getSnapshot().status === "ready" && all.getSnapshot().status === "ready");
    expect(narrowed.getSnapshot().items.map((i) => i.title)).toEqual(["next"]);

    const calls = daemon.calls.listItems;
    daemon.updateView(next.id, { filter: statusIn(Status.INBOX) });
    await waitFor(() => narrowed.getSnapshot().items[0]?.title === "inboxed");
    await sleep(40);
    expect(daemon.calls.listItems).toBe(calls + 1);
    expect(all.getSnapshot().items).toHaveLength(2);
  });

  it("shows soft-deleted Items in a view whose saved filter includes them", async () => {
    // No re-query within the test: what the views show comes from the store.
    const { daemon, store } = setup(2, { debounceMs: 60_000 });
    const inbox = saved(daemon, "inbox");
    daemon.updateView(inbox.id, { filter: { ...statusIn(Status.INBOX), includeDeleted: true } });
    const items = createClient(ItemService, daemon.transport);
    const withDeleted = store.view({ viewId: inbox.id });
    const plain = store.view({ filter: statusIn(Status.INBOX) });
    store.start();
    await waitFor(() => withDeleted.getSnapshot().items.length === 2 && plain.getSnapshot().items.length === 2);
    const [a] = plain.getSnapshot().items;

    await deleteItem(items, a!.id);
    await waitFor(() => store.getItem(a!.id)?.deletedAt !== undefined);
    expect(plain.getSnapshot().items.map((i) => i.id)).not.toContain(a!.id);
    expect(withDeleted.getSnapshot().items.map((i) => i.id)).toContain(a!.id);
  });

  it("keeps the status and tab counts live while they are observed", async () => {
    const { daemon, store } = setup(2);
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    daemon.put({ title: "unobserved" });
    await sleep(60);
    expect(daemon.calls.getStatusCounts).toBe(0);
    expect(store.getCounts().status).toBe("loading");

    const unsubscribe = store.subscribeCounts(() => {});
    await waitFor(() => store.getCounts().status === "ready");
    const inbox = saved(daemon, "inbox").id;
    const counts = store.getCounts();
    expect(counts.counts).toMatchObject({ inbox: 3, due: 0, overdue: 0 });
    expect([...counts.views]).toEqual(["inbox", "today", "next", "waiting"].map((seed) => [saved(daemon, seed).id, seed === "inbox" ? 3 : 0]));
    expect(counts.today).toBeDefined();
    expect(store.getCounts()).toBe(counts);

    // Item changes re-count, debounced.
    const calls = daemon.calls.getStatusCounts;
    for (let i = 0; i < 5; i++) daemon.put({ title: `burst ${i}` });
    await waitFor(() => store.getCounts().counts?.inbox === 8);
    await sleep(40);
    expect(daemon.calls.getStatusCounts).toBe(calls + 1);
    // So do view changes.
    daemon.updateView(inbox, { showCount: false });
    await waitFor(() => !store.getCounts().views.has(inbox));

    unsubscribe();
    const after = daemon.calls.getStatusCounts;
    daemon.put({ title: "unobserved again" });
    await sleep(60);
    expect(daemon.calls.getStatusCounts).toBe(after);
    // The next observer reloads them.
    const again = store.subscribeCounts(() => {});
    await waitFor(() => store.getCounts().counts?.inbox === 9);
    again();
  });

  it("asks for MAX_PAGE_SIZE pages", async () => {
    const { daemon, store } = setup(3);
    const sizes: number[] = [];
    daemon.onListItems = (req) => void sizes.push(req.pageSize);
    store.view();
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    expect(sizes).toEqual([MAX_PAGE_SIZE]);
  });

  it("keeps the first of an Item a later page returns again", async () => {
    // No re-query within the test: the ids are the first load's.
    const { daemon, store } = setup(0, { pageSize: 2, debounceMs: 60_000 });
    const [a, b, c, d] = [Priority.HIGH, Priority.MEDIUM, Priority.LOW, Priority.NONE].map((priority, i) =>
      daemon.put({ title: `p${i}`, status: Status.NEXT, priority }),
    );
    let pages = 0;
    daemon.onListItems = (req) => {
      // Between the first and second page, b moves past the cursor.
      if (req.pageToken !== "" && pages++ === 0) daemon.update(b!.id, { priority: Priority.NONE });
    };
    const view = store.view({ filter: statusIn(Status.NEXT), sort: [{ key: ItemField.EFFECTIVE_PRIORITY, descending: true }] });
    store.start();
    await waitFor(() => view.getSnapshot().status === "ready");
    expect(daemon.calls.listItems).toBe(3);
    expect(view.ids).toEqual([a!.id, b!.id, c!.id, d!.id]);
    expect(view.getSnapshot().items.map((i) => i.id)).toEqual([a!.id, b!.id, c!.id, d!.id]);
  });

  it("applies no message once stopped, so no re-query timer outlives stop", async () => {
    const daemon = new FakeDaemon([{ title: "a" }]);
    // Stops the store as a change arrives, before the store handles it.
    let onChange: (() => void) | undefined;
    const transport: Transport = {
      unary: (...args) => daemon.transport.unary(...args),
      async stream(...args) {
        const res = await daemon.transport.stream(...args);
        const message = (async function* () {
          for await (const m of res.message) {
            if ((m as SubscribeResponse).message.case === "change") onChange?.();
            yield m;
          }
        })();
        return { ...res, message } as typeof res;
      },
    };
    const store = new LiveStore({ transport, debounceMs: 777 });
    stores.push(store);
    store.view();
    const unsubscribe = store.subscribeCounts(() => {});
    store.start();
    await waitFor(() => store.getConnection().state === "live" && store.getCounts().status === "ready");
    const timers = vi.spyOn(globalThis, "setTimeout");
    let stopped: Promise<void> | undefined;
    onChange = () => {
      stopped ??= store.stop();
    };
    const late = daemon.put({ title: "late" });
    await waitFor(() => stopped !== undefined);
    await stopped;
    const debounces = timers.mock.calls.filter(([, ms]) => ms === 777).length;
    timers.mockRestore();
    unsubscribe();
    expect(debounces).toBe(0);
    expect(store.getItem(late.id)).toBeUndefined();
  });
});
