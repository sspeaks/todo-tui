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
import { addRelation, deleteItem, removeRelation, setStatus, undeleteItem, undo } from "../src/mutations.js";

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

  it("asks for the daemon again in each later outage, and backs off from the start again", async () => {
    let spawned = 0;
    const { daemon, store } = setup(1, {
      ensureDaemon: async () => {
        spawned++;
      },
      backoff: { initialMs: 10, maxMs: 10_000 },
    });
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    // A live stream that then fails (the daemon killed) ends with an error, not by returning.
    for (const outage of [1, 2, 3]) {
      daemon.down = true;
      daemon.dropStreams();
      await waitFor(() => store.getConnection().attempt >= 3);
      expect(spawned).toBe(outage);
      // The backoff restarts each outage: three attempts within 10 + 20 + 40 ms, not the last outage's 10 s.
      expect(store.getConnection().attempt).toBeLessThan(5);
      daemon.down = false;
      await waitFor(() => store.getConnection().state === "live");
    }
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

  it("re-reads the views and the counts on refresh(), as a new day needs", async () => {
    let now = new Date(Date.UTC(2026, 9, 25, 23, 59));
    const daemon = new FakeDaemon([], { timeZone: "UTC", now: () => now });
    daemon.put({ title: "due tomorrow", status: Status.NEXT, dueDate: { year: 2026, month: 10, day: 26 } });
    const store = new LiveStore({ transport: daemon.transport, debounceMs: 20 });
    stores.push(store);
    const today = saved(daemon, "today").id;
    const view = store.view({ viewId: today });
    const unsubscribe = store.subscribeCounts(() => {});
    store.start();
    await waitFor(() => store.getCounts().status === "ready" && view.getSnapshot().status === "ready");
    expect(store.getCounts().today).toMatchObject({ year: 2026, month: 10, day: 25 });
    expect(store.getCounts().counts).toMatchObject({ due: 0 });
    expect(view.getSnapshot().items).toEqual([]);

    // Midnight passes, and nothing is written.
    now = new Date(Date.UTC(2026, 9, 26, 0, 1));
    store.refresh();
    await waitFor(() => store.getCounts().today?.day === 26 && view.getSnapshot().items.length === 1);
    expect(store.getCounts().counts).toMatchObject({ due: 1, overdue: 0 });
    expect(store.getCounts().views.get(today)).toBe(1);
    unsubscribe();

    // Unobserved counts are re-read by their next observer.
    const calls = daemon.calls.getStatusCounts;
    now = new Date(Date.UTC(2026, 9, 27, 0, 1));
    store.refresh();
    await sleep(40);
    expect(daemon.calls.getStatusCounts).toBe(calls);
    const again = store.subscribeCounts(() => {});
    await waitFor(() => store.getCounts().counts?.overdue === 1);
    again();
  });

  it("re-reads counts a resync found unobserved once they are observed again", async () => {
    const { daemon, store } = setup(2);
    store.start();
    const unsubscribe = store.subscribeCounts(() => {});
    await waitFor(() => store.getCounts().counts?.inbox === 2);
    unsubscribe();
    // The database is replaced by an older one with one Item.
    const [kept] = [...daemon.items.values()];
    daemon.restore(3n, [kept!]);
    daemon.dropStreams();
    await waitFor(() => daemon.calls.listViews === 2);
    await sleep(40);
    const again = store.subscribeCounts(() => {});
    await waitFor(() => store.getCounts().counts?.inbox === 1);
    again();
  });

  it("shows a view whose saved view is gone as an error with no rows, and stops re-querying it", async () => {
    const { daemon, store } = setup(1);
    const inbox = saved(daemon, "inbox").id;
    const view = store.view({ viewId: inbox });
    const unsubscribe = view.subscribe(() => {});
    store.start();
    await waitFor(() => view.getSnapshot().status === "ready");
    expect(view.getSnapshot().items.map((i) => i.title)).toEqual(["item 0"]);

    daemon.removeView(inbox);
    await waitFor(() => view.getSnapshot().status === "error");
    expect(view.getSnapshot().items).toEqual([]);
    expect(ConnectError.from(view.getSnapshot().error).code).toBe(Code.NotFound);
    const calls = daemon.calls.listItems;
    for (let i = 0; i < 3; i++) daemon.put({ title: `later ${i}` });
    await sleep(60);
    expect(daemon.calls.listItems).toBe(calls);
    unsubscribe();
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

describe("LiveStore Items on demand and the undo state", () => {
  it("applies mutation responses at once with applyItems, keeping anything newer", async () => {
    // No re-query and no stream within the test: what the store holds comes from applyItems.
    const { daemon, store } = setup(1, { debounceMs: 60_000 });
    const items = createClient(ItemService, daemon.transport);
    const view = store.view();
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    const [first] = view.getSnapshot().items;
    await store.stop();

    const res = await setStatus(items, first!.id, Status.DONE);
    const version = store.getVersion();
    store.applyItems([res.item!], res.result!.revision);
    expect(store.getVersion()).toBeGreaterThan(version);
    expect(store.getItem(first!.id)?.status).toBe(Status.DONE);
    // A response read at an older revision does not undo it.
    store.applyItems([{ ...res.item!, status: Status.INBOX }], res.result!.revision - 1n);
    expect(store.getItem(first!.id)?.status).toBe(Status.DONE);
  });

  it("fetches an Item no view holds, deleted ones too, and keeps it fresh from changes", async () => {
    const { daemon, store } = setup(0);
    const items = createClient(ItemService, daemon.transport);
    const parent = daemon.put({ title: "parent", status: Status.DONE });
    const gone = daemon.put({ title: "gone" });
    await deleteItem(items, gone.id);
    store.view({ filter: { allOf: [] } });
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    expect(store.getItem(parent.id)).toBeUndefined();

    const before = daemon.calls.listItems;
    const [a, b] = await Promise.all([store.fetchItem(parent.id), store.fetchItem(parent.id)]);
    expect(a?.title).toBe("parent");
    expect(b).toBe(a);
    expect(store.getItem(parent.id)).toBe(a);
    expect(daemon.calls.listItems).toBe(before);
    expect((await store.fetchItem(gone.id))?.deletedAt).toBeDefined();
    expect(await store.fetchItem("01J00000000000000000000000")).toBeUndefined();

    daemon.update(parent.id, { title: "renamed parent" });
    await waitFor(() => store.getItem(parent.id)?.title === "renamed parent");
  });

  it("rejects a fetch the daemon cannot answer", async () => {
    const { daemon, store } = setup(1);
    daemon.down = true;
    await expect(store.fetchItem("01J00000000000000000000000")).rejects.toMatchObject({ code: Code.Unavailable });
  });

  it("keeps the undo state live while observed, re-reading it after Item changes", async () => {
    const { daemon, store } = setup(1);
    const items = createClient(ItemService, daemon.transport);
    const view = store.view();
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    expect(store.undoState.getSnapshot().status).toBe("loading");

    const unsubscribe = store.undoState.subscribe(() => {});
    await waitFor(() => store.undoState.getSnapshot().status === "ready");
    expect(store.undoState.getSnapshot().undo).toBeUndefined();

    const [first] = view.getSnapshot().items;
    const res = await setStatus(items, first!.id, Status.DONE);
    await waitFor(() => store.undoState.getSnapshot().undo?.opId === res.result?.opId);
    expect(store.undoState.getSnapshot().undo?.description).toBe(`mark done "item 0"`);

    await undo(items, { expectedOpId: res.result!.opId });
    await waitFor(() => store.undoState.getSnapshot().redo?.opId === res.result?.opId);
    expect(store.undoState.getSnapshot().undo).toBeUndefined();
    unsubscribe();
  });

  it("re-reads the undo state on reload(), observed or not, and only on demand while unobserved", async () => {
    const { daemon, store } = setup(1);
    const items = createClient(ItemService, daemon.transport);
    const view = store.view();
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    const [first] = view.getSnapshot().items;
    const res = await setStatus(items, first!.id, Status.DONE);
    await sleep(60);
    // Unobserved: changes do not re-read it.
    expect(store.undoState.getSnapshot().status).toBe("loading");

    await store.undoState.reload();
    expect(store.undoState.getSnapshot().undo?.opId).toBe(res.result?.opId);
  });

  it("applies the newest undo state read when reads overlap", async () => {
    const { daemon, store } = setup(1);
    const items = createClient(ItemService, daemon.transport);
    const view = store.view();
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    const [first] = view.getSnapshot().items;
    // The older read sees the stack before the op, and its answer arrives last.
    const before = await store.fetchUndoState();
    let answer = () => {};
    vi.spyOn(store, "fetchUndoState").mockImplementationOnce(() => new Promise((resolve) => (answer = () => resolve(before))));
    const older = store.undoState.reload();
    const res = await setStatus(items, first!.id, Status.DONE);
    await store.undoState.reload();
    expect(store.undoState.getSnapshot().undo?.opId).toBe(res.result?.opId);
    answer();
    await older;
    expect(store.undoState.getSnapshot().undo?.opId).toBe(res.result?.opId);
  });

  it("stops re-reading the undo state when the store stops", async () => {
    const { daemon, store } = setup(1, { debounceMs: 200 });
    store.view();
    const unsubscribe = store.undoState.subscribe(() => {});
    store.start();
    await waitFor(() => store.undoState.getSnapshot().status === "ready");
    const reads = vi.spyOn(store, "fetchUndoState");
    const late = daemon.put({ title: "late" });
    await waitFor(() => store.getItem(late.id) !== undefined);
    await store.stop();
    await sleep(300);
    expect(reads).not.toHaveBeenCalled();
    unsubscribe();
  });

  it("shows a first undo-state failure as an error", async () => {
    const { daemon, store } = setup(1);
    store.view();
    store.start();
    await waitFor(() => store.getConnection().state === "live");
    daemon.down = true;
    await store.undoState.reload();
    expect(store.undoState.getSnapshot()).toMatchObject({ status: "error" });
    daemon.down = false;
    await store.undoState.reload();
    expect(store.undoState.getSnapshot()).toMatchObject({ status: "ready" });
  });
});
