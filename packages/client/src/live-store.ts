import { type MessageInitShape, create, toBinary } from "@bufbuild/protobuf";
import { type Client, type Transport, createClient } from "@connectrpc/connect";
import { Topic } from "./gen/todo/v1/common_pb.js";
import type { FilterSchema, SortKeySchema } from "./gen/todo/v1/filter_pb.js";
import type { Item } from "./gen/todo/v1/item_pb.js";
import { ItemService, type ListItemsRequest, ListItemsRequestSchema } from "./gen/todo/v1/item_service_pb.js";
import { type Change, type Notice, WatchService } from "./gen/todo/v1/watch_service_pb.js";
import { isConnectionFailure } from "./spawn.js";

export interface ViewQuery {
  filter?: MessageInitShape<typeof FilterSchema>;
  sort?: MessageInitShape<typeof SortKeySchema>[];
  /** Narrow a saved view (ListItemsRequest.view_id). */
  viewId?: string;
}

export type ConnectionState = "idle" | "connecting" | "live" | "reconnecting" | "closed";

export interface ConnectionSnapshot {
  readonly state: ConnectionState;
  /** Last revision applied from the stream (or the initial load). */
  readonly revision: bigint;
  /** Consecutive failed attempts since the stream was last healthy. */
  readonly attempt: number;
  readonly error?: unknown;
}

export interface ViewSnapshot {
  readonly status: "loading" | "ready" | "error";
  readonly items: readonly Item[];
  /** Revision of the ListItems result the order came from. */
  readonly revision: bigint;
  readonly error?: unknown;
}

export interface LiveStoreOptions {
  transport: Transport;
  /** Delay before re-running a view's ListItems after item changes (default 150). */
  debounceMs?: number;
  /** Reconnect backoff: initialMs doubling up to maxMs (default 100 / 5000). */
  backoff?: { initialMs?: number; maxMs?: number };
  pageSize?: number;
  /** Called once per outage when the daemon is unreachable (e.g. respawn it). */
  ensureDaemon?: () => Promise<void>;
  /** How long an unobserved view is kept before it is dropped (default 5000). */
  viewGraceMs?: number;
}

type Listener = () => void;

function abortableSleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const t = setTimeout(done, ms);
    signal.addEventListener("abort", done, { once: true });
    function done() {
      clearTimeout(t);
      signal.removeEventListener("abort", done);
      resolve();
    }
  });
}

/**
 * Normalized client-side cache fed by ListItems and WatchService.Subscribe
 * (design 4.4), exposed for useSyncExternalStore.
 */
export class LiveStore {
  private readonly items = new Map<string, Item>();
  /** Highest revision each id was observed at; outlives deletes as a tombstone. */
  private readonly seen = new Map<string, bigint>();
  private readonly views = new Map<string, LiveView>();
  private readonly listeners = new Set<Listener>();
  private readonly noticeListeners = new Set<(n: Notice) => void>();
  private readonly itemsClient: Client<typeof ItemService>;
  private readonly watchClient: Client<typeof WatchService>;
  private revision: bigint | undefined;
  /** Set while a ResyncRequired reload has not completed; retried on CaughtUp. */
  private pendingResync = false;
  private version = 0;
  private conn: ConnectionSnapshot = { state: "idle", revision: 0n, attempt: 0 };
  private abort: AbortController | undefined;
  private runLoop: Promise<void> | undefined;

  readonly debounceMs: number;
  readonly pageSize: number;
  readonly viewGraceMs: number;

  constructor(private readonly opts: LiveStoreOptions) {
    this.itemsClient = createClient(ItemService, opts.transport);
    this.watchClient = createClient(WatchService, opts.transport);
    this.debounceMs = opts.debounceMs ?? 150;
    this.pageSize = opts.pageSize ?? 0;
    this.viewGraceMs = opts.viewGraceMs ?? 5_000;
  }

  get started(): boolean {
    return this.abort !== undefined && !this.abort.signal.aborted;
  }

  /** Loads the current views, then subscribes from the loaded revision and stays live. */
  start(): void {
    if (this.abort) return;
    this.abort = new AbortController();
    this.setConn({ state: "connecting", attempt: 0 });
    this.runLoop = this.run(this.abort.signal);
  }

  async stop(): Promise<void> {
    if (!this.abort || this.abort.signal.aborted) return;
    this.abort.abort();
    for (const v of this.views.values()) v.cancelTimers();
    await this.runLoop;
    this.setConn({ state: "closed" });
  }

  // --- useSyncExternalStore plumbing ---

  subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  /** Bumped on every state change. */
  getVersion = (): number => this.version;

  getConnection = (): ConnectionSnapshot => this.conn;

  getItem = (id: string): Item | undefined => this.items.get(id);

  onNotice(listener: (n: Notice) => void): () => void {
    this.noticeListeners.add(listener);
    return () => {
      this.noticeListeners.delete(listener);
    };
  }

  /** The live view for a query; equal queries share one view. */
  view(query: ViewQuery = {}): LiveView {
    const request = create(ListItemsRequestSchema, {
      ...(query.filter !== undefined && { filter: query.filter }),
      ...(query.sort !== undefined && { sort: query.sort }),
      ...(query.viewId !== undefined && { viewId: query.viewId }),
    });
    const key = Buffer.from(toBinary(ListItemsRequestSchema, request)).toString("base64");
    let view = this.views.get(key);
    if (!view) {
      view = new LiveView(this, key, request);
      this.views.set(key, view);
      if (this.revision !== undefined && this.started) void view.load();
    }
    return view;
  }

  // --- internals shared with LiveView ---

  /** @internal */
  bump(): void {
    this.version++;
    for (const l of [...this.listeners]) l();
  }

  /** @internal */
  async listAll(request: ListItemsRequest): Promise<{ items: Item[]; revision: bigint }> {
    const items: Item[] = [];
    let revision: bigint | undefined;
    let pageToken = "";
    do {
      const res = await this.itemsClient.listItems(
        { ...request, pageSize: this.pageSize, pageToken },
        this.abort ? { signal: this.abort.signal } : {},
      );
      revision ??= res.revision;
      items.push(...res.items);
      pageToken = res.nextPageToken;
    } while (pageToken !== "");
    return { items, revision: revision ?? 0n };
  }

  /** @internal Applies a ListItems result read at `revision`, skipping anything already newer. */
  applyList(items: readonly Item[], revision: bigint): void {
    for (const item of items) {
      const seen = this.seen.get(item.id);
      if (seen === undefined || seen <= revision) {
        this.seen.set(item.id, revision);
        this.items.set(item.id, item);
      }
    }
  }

  /** @internal */
  dropView(key: string): void {
    this.views.get(key)?.cancelTimers();
    this.views.delete(key);
  }

  private setConn(patch: Partial<ConnectionSnapshot>): void {
    const next = { ...this.conn, ...patch, revision: this.revision ?? 0n };
    if (patch.state === "live" || patch.state === "connecting") delete (next as { error?: unknown }).error;
    this.conn = next;
    this.bump();
  }

  private async run(signal: AbortSignal): Promise<void> {
    let attempt = 0;
    let spawnedThisOutage = false;
    while (!signal.aborted) {
      let healthy = false;
      try {
        if (this.revision === undefined) await this.loadBaseline();
        healthy = await this.consume(signal);
      } catch (err) {
        if (signal.aborted) break;
        if (isConnectionFailure(err) && this.opts.ensureDaemon && !spawnedThisOutage) {
          spawnedThisOutage = true;
          await this.opts.ensureDaemon().catch(() => {});
        }
        this.conn = { ...this.conn, error: err };
      }
      if (signal.aborted) break;
      if (healthy) {
        attempt = 0;
        spawnedThisOutage = false;
      }
      attempt++;
      this.setConn({ state: "reconnecting", attempt });
      const initial = this.opts.backoff?.initialMs ?? 100;
      const max = this.opts.backoff?.maxMs ?? 5_000;
      await abortableSleep(Math.min(initial * 2 ** (attempt - 1), max), signal);
    }
  }

  /**
   * Loads every view and takes the lowest read revision as the subscribe
   * point. A view the daemon rejects (e.g. UNIMPLEMENTED) is left in `error`
   * and does not hold up the others; only connection failures abort.
   */
  private async loadBaseline(): Promise<void> {
    const results = await Promise.allSettled([...this.views.values()].map((v) => v.load(true)));
    const revs: bigint[] = [];
    for (const r of results) {
      if (r.status === "fulfilled") revs.push(r.value);
      else if (isConnectionFailure(r.reason)) throw r.reason;
    }
    this.revision =
      revs.length > 0
        ? revs.reduce((a, b) => (b < a ? b : a))
        : (await this.itemsClient.listItems({ pageSize: 1 })).revision;
  }

  /** Subscribes from the current revision. Returns true when the stream got any message. */
  private async consume(signal: AbortSignal): Promise<boolean> {
    let healthy = false;
    const stream = this.watchClient.subscribe({ sinceRevision: this.revision ?? 0n, topics: [] }, { signal });
    for await (const res of stream) {
      if (!healthy) healthy = true;
      const msg = res.message;
      switch (msg.case) {
        case "change":
          this.applyChange(msg.value);
          break;
        case "caughtUp":
          if (this.revision === undefined || msg.value.revision > this.revision) this.revision = msg.value.revision;
          this.setConn({ state: "live", attempt: 0 });
          if (this.pendingResync) void this.reloadAll(this.revision);
          else for (const v of this.views.values()) if (v.needsLoad) void v.load();
          break;
        case "resyncRequired":
          // The server's history no longer connects to ours (changes expired,
          // or the database was replaced and its revision went backwards), so
          // per-id revisions from before mean nothing: forget them.
          this.revision = msg.value.currentRevision;
          this.seen.clear();
          this.pendingResync = true;
          break;
        case "notice":
          for (const l of [...this.noticeListeners]) l(msg.value);
          break;
      }
    }
    return healthy;
  }

  private applyChange(change: Change): void {
    if (this.revision === undefined || change.revision > this.revision) this.revision = change.revision;
    if (change.topic !== Topic.ITEM) {
      this.conn = { ...this.conn, revision: this.revision };
      return;
    }
    const seen = this.seen.get(change.id);
    if (seen === undefined || change.revision >= seen) {
      this.seen.set(change.id, change.revision);
      if (change.deleted) {
        this.items.delete(change.id);
      } else if (change.resource.case === "item") {
        this.items.set(change.id, change.resource.value);
      }
    }
    this.conn = { ...this.conn, revision: this.revision };
    for (const v of this.views.values()) v.scheduleRequery();
    this.bump();
  }

  /**
   * After ResyncRequired: reloads every view, then forgets items no view
   * holds unless a change after `atLeast` vouches for them. If the daemon
   * goes away meanwhile, the resync stays pending and the next CaughtUp
   * retries it; a view the daemon rejects keeps its last ids.
   */
  private async reloadAll(atLeast: bigint): Promise<void> {
    this.pendingResync = false;
    const results = await Promise.allSettled([...this.views.values()].map((v) => v.load(true)));
    if (results.some((r) => r.status === "rejected" && isConnectionFailure(r.reason))) {
      this.pendingResync = true;
      return;
    }
    const held = new Set<string>();
    for (const v of this.views.values()) for (const id of v.ids) held.add(id);
    for (const id of [...this.items.keys(), ...this.seen.keys()]) {
      const rev = this.seen.get(id);
      if (!held.has(id) && (rev === undefined || rev <= atLeast)) {
        this.items.delete(id);
        this.seen.delete(id);
      }
    }
    this.bump();
  }
}

/** One ordered ListItems query kept live by its LiveStore. */
export class LiveView {
  ids: readonly string[] = [];
  private status: ViewSnapshot["status"] = "loading";
  private error: unknown;
  private revision = 0n;
  private loaded = false;
  /** A reload failed after the view had loaded; its ids may be out of date. */
  private stale = false;
  private seq = 0;
  private debounce: ReturnType<typeof setTimeout> | undefined;
  private grace: ReturnType<typeof setTimeout> | undefined;
  private refs = 0;
  private snap: ViewSnapshot = { status: "loading", items: [], revision: 0n };
  private snapVersion = -1;

  constructor(
    private readonly store: LiveStore,
    private readonly key: string,
    /** @internal */
    readonly request: ListItemsRequest,
  ) {}

  get needsLoad(): boolean {
    return !this.loaded || this.status === "error" || this.stale;
  }

  /** Runs ListItems; out-of-order responses are discarded. Returns the read revision. */
  async load(rethrow = false): Promise<bigint> {
    const seq = ++this.seq;
    try {
      const { items, revision } = await this.store.listAll(this.request);
      if (seq === this.seq) {
        this.store.applyList(items, revision);
        this.ids = items.map((i) => i.id);
        this.revision = revision;
        this.status = "ready";
        this.error = undefined;
        this.loaded = true;
        this.stale = false;
        this.store.bump();
      }
      return revision;
    } catch (err) {
      if (seq === this.seq) {
        if (this.loaded) {
          this.stale = true;
        } else {
          this.status = "error";
          this.error = err;
          this.store.bump();
        }
      }
      if (rethrow) throw err;
      return this.revision;
    }
  }

  /** @internal */
  scheduleRequery(): void {
    if (this.debounce) clearTimeout(this.debounce);
    this.debounce = setTimeout(() => {
      this.debounce = undefined;
      void this.load();
    }, this.store.debounceMs);
  }

  /** @internal */
  cancelTimers(): void {
    if (this.debounce) clearTimeout(this.debounce);
    if (this.grace) clearTimeout(this.grace);
    this.debounce = this.grace = undefined;
  }

  /** Keeps the view alive while subscribed; also usable with useSyncExternalStore. */
  subscribe = (listener: () => void): (() => void) => {
    this.refs++;
    if (this.grace) {
      clearTimeout(this.grace);
      this.grace = undefined;
    }
    const unsub = this.store.subscribe(listener);
    return () => {
      unsub();
      if (--this.refs === 0) {
        this.grace = setTimeout(() => this.store.dropView(this.key), this.store.viewGraceMs);
      }
    };
  };

  /** Referentially stable until the view's visible contents change. */
  getSnapshot = (): ViewSnapshot => {
    const version = this.store.getVersion();
    if (version === this.snapVersion) return this.snap;
    this.snapVersion = version;
    const items: Item[] = [];
    for (const id of this.ids) {
      const item = this.store.getItem(id);
      if (item) items.push(item);
    }
    const prev = this.snap;
    const same =
      prev.status === this.status &&
      prev.revision === this.revision &&
      prev.error === this.error &&
      prev.items.length === items.length &&
      prev.items.every((it, i) => it === items[i]);
    if (!same) {
      this.snap = {
        status: this.status,
        items,
        revision: this.revision,
        ...(this.error !== undefined && { error: this.error }),
      };
    }
    return this.snap;
  };
}
