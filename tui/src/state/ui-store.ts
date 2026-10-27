import { type UiState, initialUiState } from "./types.js";

/**
 * The UI state as a tiny external store (Decision 2): the dispatcher and
 * the action queue read the latest state synchronously, however many keys
 * one input event carries, and components read it via useSyncExternalStore.
 */
export class UiStore {
  private state: UiState;
  private readonly listeners = new Set<() => void>();

  constructor(state: UiState = initialUiState()) {
    this.state = state;
  }

  getState = (): UiState => this.state;

  /** Replaces the state with `fn`'s result; returning the same object notifies nobody. */
  setState(fn: (s: UiState) => UiState): void {
    const next = fn(this.state);
    if (next === this.state) return;
    this.state = next;
    for (const l of [...this.listeners]) l();
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
}
