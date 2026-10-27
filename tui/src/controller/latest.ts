/**
 * One request in flight, the latest input winning (Decision 7): no debounce
 * timer, so a preview follows every keystroke and tests need no fake
 * clock. Input that arrives while a request runs waits; when the request
 * settles only the newest waiting input is sent. `apply` gets every
 * response with the input it answers, and must drop one that no longer
 * matches what is typed.
 */
export class Latest<I, O> {
  private running = false;
  private next: { input: I } | undefined;
  private idlers: (() => void)[] = [];

  constructor(
    private readonly fetch: (input: I) => Promise<O>,
    private readonly apply: (input: I, out: O) => void,
    private readonly onError: (input: I, err: unknown) => void = () => {},
  ) {}

  request(input: I): void {
    if (this.running) this.next = { input };
    else void this.send(input);
  }

  /** Whether a request is in flight. */
  get busy(): boolean {
    return this.running;
  }

  /** Resolves once nothing is in flight or waiting. */
  idle(): Promise<void> {
    return this.running ? new Promise((resolve) => this.idlers.push(resolve)) : Promise.resolve();
  }

  private async send(input: I): Promise<void> {
    this.running = true;
    let cur: { input: I } | undefined = { input };
    while (cur) {
      try {
        this.apply(cur.input, await this.fetch(cur.input));
      } catch (err) {
        this.onError(cur.input, err);
      }
      cur = this.next;
      this.next = undefined;
    }
    this.running = false;
    for (const resolve of this.idlers.splice(0)) resolve();
  }
}
