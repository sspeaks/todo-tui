// The serial action pipeline (Decision 2, m4_arch §3.2). An action runs
// inside the input queue, so every key waits behind it, and in order:
//   1. the RPC (after a connection failure: wait for live, then the same
//      request again, with the same request_id);
//   2. the response's Items into the store;
//   3. a reload of the current view and of the undo state, awaited;
// and the caller then toasts and the next key runs. There is no client-side
// optimism: the next key sees the list as the daemon left it, so `xx`
// completes two Items, never one twice. An edit of someone else's issue
// (NOT_MINE, design 5.9) asks first, within step 1: y sends it again with
// confirm_not_mine, and the action goes on as if it had not asked.

import { type CallOptions, type Item, type LiveStore, type LiveView, errorReason, isConnectionFailure, ulid } from "@todo-tui/client";
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

/** What a mutation helper takes: the request id to reuse, the deadline, and the say-so for someone else's issue. */
export type CallOpts = CallOptions & { requestId: string; confirmNotMine?: boolean };

/** Asks whether to change someone else's issue (`err` names it) in the tracker; resolves to the answer. */
export type AskNotMine = (err: unknown, targets: readonly string[] | undefined) => Promise<boolean>;

export interface Effect<R> {
  /** The RPC; called again with the same options after a connection failure. */
  readonly call: (opts: CallOpts) => Promise<R>;
  /** The request_id to send (default: a new one), e.g. the one of a capture whose outcome is unknown. */
  readonly requestId?: string;
  /** The Items the response carries, and the revision they were read at. */
  readonly items: (res: R) => { readonly items: readonly Item[]; readonly revision: bigint };
  /** The Items it acts on, when several (a batch): the NOT_MINE question counts those not the person's. */
  readonly targets?: readonly string[];
  /** The person already said yes to changing someone else's issue (a retry of an action they confirmed). */
  readonly confirmed?: boolean;
}

export type Outcome<R> =
  /** `confirmed`: it was sent with the person's say-so for someone else's issue, which a retry keeps. */
  | { readonly ok: true; readonly res: R; readonly confirmed: boolean }
  | { readonly ok: false; readonly err: unknown; readonly confirmed: boolean };

/** The Items and revision of a single-Item mutation's response, with the next instance of a series it completed (SetStatus). */
export const single = <R extends { item?: Item; generated?: Item; result?: { revision: bigint } }>(res: R) => ({
  items: [res.item, res.generated].filter((i) => i !== undefined),
  revision: res.result?.revision ?? 0n,
});

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, Math.max(0, ms)));

export class Pipeline {
  constructor(
    private readonly store: LiveStore,
    private readonly timing: Timing,
    /** The current tab's views (the rows shown, and its own), reloaded after each action. */
    private readonly views: () => readonly LiveView[],
    /** Sets (or, with undefined, clears) the status bar's "saving…". */
    private readonly busy: (label: string | undefined) => void,
    /** Asks the NOT_MINE question; without it a NOT_MINE refusal is the action's error. */
    private readonly askNotMine?: AskNotMine,
  ) {}

  /** Runs one action through steps 1–3; never throws. */
  async run<R>(effect: Effect<R>): Promise<Outcome<R>> {
    let saving = setTimeout(() => this.busy("saving…"), this.timing.savingMs);
    let confirmed = effect.confirmed ?? false;
    const send = (opts: CallOpts) => effect.call(confirmed ? { ...opts, confirmNotMine: true } : opts);
    try {
      const requestId = effect.requestId ?? ulid();
      let res: R;
      try {
        res = await this.call(send, requestId);
      } catch (err) {
        if (confirmed || errorReason(err) !== "NOT_MINE" || !this.askNotMine) throw err;
        clearTimeout(saving);
        this.busy(undefined);
        if (!(await this.askNotMine(err, effect.targets))) throw err;
        confirmed = true;
        saving = setTimeout(() => this.busy("saving…"), this.timing.savingMs);
        // Refused, so nothing was recorded for the request id: the same one.
        res = await this.call(send, requestId);
      }
      const { items, revision } = effect.items(res);
      this.store.applyItems(items, revision);
      await this.settle(revision);
      return { ok: true, res, confirmed };
    } catch (err) {
      return { ok: false, err, confirmed };
    } finally {
      clearTimeout(saving);
      this.busy(undefined);
    }
  }

  /**
   * An RPC that must not be sent twice (promote makes a Jira issue each
   * time), shown as `label` while it runs: sent once with `timeoutMs`, its
   * Items into the store and the views settled after. A failure is thrown,
   * never resent.
   */
  async once<R>(label: string, timeoutMs: number, call: (opts: CallOpts) => Promise<R>, items: Effect<R>["items"]): Promise<R> {
    this.busy(label);
    try {
      const res = await call({ requestId: ulid(), timeoutMs });
      const { items: got, revision } = items(res);
      this.store.applyItems(got, revision);
      await this.settle(revision);
      return res;
    } finally {
      this.busy(undefined);
    }
  }

  /**
   * Re-reads the current views and the undo state: the next key sees the
   * list as the action left it (read at `revision` or later), and u names
   * the action.
   */
  async settle(revision = 0n): Promise<void> {
    await Promise.all([...this.views().map((v) => this.reloadView(v, revision)), this.store.undoState.reload()]);
  }

  /**
   * LiveView.load, again while the rows are older than `revision`: a load
   * the change's own debounced requery starts meanwhile discards this
   * one's rows, and a failed load keeps the old ones.
   */
  private async reloadView(view: LiveView, revision: bigint): Promise<void> {
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
  async call<R>(call: (opts: CallOpts) => Promise<R>, requestId: string = ulid()): Promise<R> {
    const opts: CallOpts = { requestId, timeoutMs: this.timing.rpcTimeoutMs };
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
