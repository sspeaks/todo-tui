// The serial action pipeline (Decision 2, m4_arch §3.2). An action runs
// inside the input queue, so every key waits behind it, and in order:
//   1. the RPC (after a connection failure: wait for live, then the same
//      request again, with the same request_id);
//   2. the response's Items into the store;
//   3. a reload of the current view and of the undo state, awaited;
// and the caller then toasts and the next key runs. There is no client-side
// optimism: the next key sees the list as the daemon left it, so `xx`
// completes two Items, never one twice.

import { type CallOptions, type Item, type LiveStore, type LiveView, isConnectionFailure, ulid } from "@todo-tui/client";
import { DaemonUnavailable } from "./messages.js";

export interface Timing {
  /** "saving…" shows once an action has run this long. */
  readonly savingMs: number;
  /** After a connection failure, how long an action waits for the daemon to be live again. */
  readonly retryMs: number;
  /** Each RPC's deadline. */
  readonly rpcTimeoutMs: number;
  /** How long quitting waits for an action in flight. */
  readonly quitMs: number;
}

export const DEFAULT_TIMING: Timing = { savingMs: 250, retryMs: 5_000, rpcTimeoutMs: 5_000, quitMs: 1_000 };

/** Between a connection failure and the retry: the stream may not have noticed the outage yet, and still say live. */
const RETRY_BEAT_MS = 100;
/** How many times an action's settle reads the view before it goes on with what it has. */
const SETTLE_TRIES = 3;

/** What a mutation helper takes: the request id to reuse, and the deadline. */
export type CallOpts = CallOptions & { requestId: string };

export interface Effect<R> {
  /** The RPC; called again with the same options after a connection failure. */
  readonly call: (opts: CallOpts) => Promise<R>;
  /** The Items the response carries, and the revision they were read at. */
  readonly items: (res: R) => { readonly items: readonly Item[]; readonly revision: bigint };
}

export type Outcome<R> = { readonly ok: true; readonly res: R } | { readonly ok: false; readonly err: unknown };

/** The Items and revision of a single-Item mutation's response. */
export const single = <R extends { item?: Item; result?: { revision: bigint } }>(res: R) => ({
  items: res.item ? [res.item] : [],
  revision: res.result?.revision ?? 0n,
});

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, Math.max(0, ms)));

export class Pipeline {
  constructor(
    private readonly store: LiveStore,
    private readonly timing: Timing,
    /** The current tab's view, reloaded after each action. */
    private readonly view: () => LiveView | undefined,
    /** Sets (or, with undefined, clears) the status bar's "saving…". */
    private readonly busy: (label: string | undefined) => void,
  ) {}

  /** Runs one action through steps 1–3; never throws. */
  async run<R>(effect: Effect<R>): Promise<Outcome<R>> {
    const saving = setTimeout(() => this.busy("saving…"), this.timing.savingMs);
    try {
      const res = await this.call(effect.call);
      const { items, revision } = effect.items(res);
      this.store.applyItems(items, revision);
      await this.settle(revision);
      return { ok: true, res };
    } catch (err) {
      return { ok: false, err };
    } finally {
      clearTimeout(saving);
      this.busy(undefined);
    }
  }

  /**
   * Re-reads the current view and the undo state: the next key sees the
   * list as the action left it (read at `revision` or later), and u names
   * the action.
   */
  async settle(revision = 0n): Promise<void> {
    await Promise.all([this.reloadView(revision), this.store.undoState.reload()]);
  }

  /**
   * LiveView.load, again while the rows are older than `revision`: a load
   * the change's own debounced requery starts meanwhile discards this
   * one's rows, and a failed load keeps the old ones.
   */
  private async reloadView(revision: bigint): Promise<void> {
    const view = this.view();
    if (!view) return;
    for (let tries = 0; tries < SETTLE_TRIES; tries++) {
      await view.load();
      if (view.getSnapshot().revision >= revision) return;
    }
  }

  /**
   * One RPC with a deadline. After a connection failure it waits for the
   * stream to be live again and sends the same request, request_id and all,
   * so a request that reached the daemon before the connection broke
   * applies once (Decision 6); after `retryMs` it gives up with
   * DaemonUnavailable.
   */
  async call<R>(call: (opts: CallOpts) => Promise<R>): Promise<R> {
    const opts: CallOpts = { requestId: ulid(), timeoutMs: this.timing.rpcTimeoutMs };
    let deadline: number | undefined;
    for (;;) {
      try {
        return await call(opts);
      } catch (err) {
        if (!isConnectionFailure(err)) throw err;
        deadline ??= Date.now() + this.timing.retryMs;
        if (!(await this.waitLive(deadline))) throw new DaemonUnavailable(err);
      }
    }
  }

  /** Waits a beat, then until the stream is live; false once `deadline` passes first. */
  private async waitLive(deadline: number): Promise<boolean> {
    await sleep(Math.min(RETRY_BEAT_MS, deadline - Date.now()));
    while (Date.now() < deadline) {
      if (this.store.getConnection().state === "live") return true;
      await new Promise<void>((resolve) => {
        const timer = setTimeout(done, deadline - Date.now());
        const unsubscribe = this.store.subscribe(() => {
          if (this.store.getConnection().state === "live") done();
        });
        function done() {
          clearTimeout(timer);
          unsubscribe();
          resolve();
        }
      });
    }
    return false;
  }
}
