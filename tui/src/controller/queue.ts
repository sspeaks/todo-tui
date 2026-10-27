import type { KeyToken } from "../keys/tokenize.js";

/** A bracketed paste: text as one piece, never keys. */
export interface Paste {
  readonly paste: string;
}

export type Input = KeyToken | Paste;

/**
 * The serial input pipeline (Decision 2): every key and paste is handled in
 * arrival order, and one whose handling is async (an RPC, from phase 2)
 * holds the rest until it settles, so each key acts on the state the keys
 * before it left. Synchronous handling runs at once, within the input event.
 */
export class InputQueue {
  private readonly queue: Input[] = [];
  private running = false;
  private idlers: (() => void)[] = [];

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

  /** Resolves once every input pushed so far has been handled. */
  idle(): Promise<void> {
    return this.running ? new Promise((resolve) => this.idlers.push(resolve)) : Promise.resolve();
  }

  private async drain(): Promise<void> {
    this.running = true;
    for (let next = this.queue.shift(); next !== undefined; next = this.queue.shift()) {
      try {
        const done = this.handle(next);
        if (done) await done;
      } catch (err) {
        this.onError(err);
      }
    }
    this.running = false;
    for (const resolve of this.idlers.splice(0)) resolve();
  }
}
