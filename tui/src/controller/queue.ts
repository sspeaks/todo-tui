import type { KeyToken } from "../keys/tokenize.js";

/** A bracketed paste: text as one piece, never keys. */
export interface Paste {
  readonly paste: string;
}

export type Input = KeyToken | Paste;

/**
 * The serial input pipeline (Decision 2): every key and paste is handled in
 * arrival order, and one whose handling is async (an action's RPC and
 * reload) holds the rest until it settles, so each key acts on the state
 * the keys before it left. Synchronous handling runs at once, within the
 * input event.
 */
export class InputQueue {
  private readonly queue: Input[] = [];
  private running = false;
  private idlers: (() => void)[] = [];
  /** The async handling in progress, if any. */
  private current: Promise<void> | undefined;

  constructor(
    private readonly handle: (input: Input) => void | Promise<void>,
    private readonly onError: (err: unknown) => void,
  ) {}

  push(inputs: readonly Input[]): void {
    this.queue.push(...inputs);
    if (!this.running) void this.drain();
  }

  /** Whether an input is being handled. */
  get busy(): boolean {
    return this.running;
  }

  /** How many inputs wait behind the one being handled. */
  get waiting(): number {
    return this.queue.length;
  }

  /** Whether an input's async handling (an action) is in progress. */
  get inFlight(): boolean {
    return this.current !== undefined;
  }

  /** Resolves once the input being handled asynchronously settles (at once when none is). */
  settled(): Promise<void> {
    return this.current ?? Promise.resolve();
  }

  /** Removes and returns the inputs waiting, e.g. to drop them after a failed action. */
  take(): Input[] {
    return this.queue.splice(0);
  }

  /** Removes and returns the inputs waiting up to the first that fails `pred`. */
  takeWhile(pred: (input: Input) => boolean): Input[] {
    const n = this.queue.findIndex((i) => !pred(i));
    return this.queue.splice(0, n < 0 ? this.queue.length : n);
  }

  /** Resolves once every input pushed so far has been handled. */
  idle(): Promise<void> {
    return this.running ? new Promise((resolve) => this.idlers.push(resolve)) : Promise.resolve();
  }

  private async drain(): Promise<void> {
    this.running = true;
    for (let next = this.queue.shift(); next !== undefined; next = this.queue.shift()) {
      try {
        const done = this.handle(next);
        if (done) {
          this.current = done.then(
            () => {},
            () => {},
          );
          await done;
        }
      } catch (err) {
        this.onError(err);
      } finally {
        this.current = undefined;
      }
    }
    this.running = false;
    for (const resolve of this.idlers.splice(0)) resolve();
  }
}
