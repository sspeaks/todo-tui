// space r and :refine (design 6.2, ADR-0010): Claude proposes cleaner
// fields for the selected Item, or the marked ones, through the person's
// own Claude Code CLI. The daemon runs it in the background: the TUI says it
// is asking, then, when the job ends, what came of it. What Claude proposes
// waits in the Suggestions pane (S), ticked; nothing changes until applied.

import { AssistantJobState, errorReason } from "@todo-tui/client";
import type { Controller } from "./controller.js";
import { errorText } from "./messages.js";

/** How often a running job is looked at; tests shorten it. */
export const timing = { pollMs: 1000 };
/** How long the TUI follows one before leaving it to the pane. */
const FOLLOW_MS = 5 * 60 * 1000;

export class Refine {
  /** The jobs being followed, so the status bar can say Claude is thinking. */
  private readonly following = new Set<string>();

  constructor(private readonly c: Controller) {}

  private show(): void {
    const refining = this.following.size;
    this.c.ui.setState((s) => ({ ...s, refining }));
  }

  async start(): Promise<void> {
    const target = this.c.target(true);
    if (!target?.ids.length) return;
    let id: string;
    try {
      const res = await this.c.store.assistantClient.refine({ itemIds: [...target.ids] }, { timeoutMs: 15_000 });
      id = res.job?.id ?? "";
    } catch (err) {
      return this.c.failed(
        errorReason(err) === "ASSISTANT_OFF"
          ? 'Claude is off: set [assistant] provider = "claude_code" in the config'
          : `No refine: ${errorText(err)}`,
      );
    }
    const n = target.ids.length;
    this.c.toast("info", `Asking Claude to refine ${n === 1 ? "it" : `${n} items`}… (S shows what it proposes)`);
    // A request that joined a job already followed: no second follower.
    if (this.following.has(id)) return;
    this.following.add(id);
    this.show();
    void this.follow(id, n);
  }

  /** Waits for the job, then says how it went. */
  private async follow(id: string, n: number): Promise<void> {
    const until = Date.now() + FOLLOW_MS;
    try {
      while (Date.now() < until) {
        await new Promise((r) => setTimeout(r, timing.pollMs));
        const job = (await this.c.store.assistantClient.getJob({ id }, { timeoutMs: 10_000 })).job;
        if (!job) return;
        if (job.state === AssistantJobState.FAILED) {
          const made = job.suggestionIds.length;
          const part = made ? ` (${made} ${made === 1 ? "item" : "items"} done first · S to review)` : "";
          return this.c.toast("error", `Claude could not refine it: ${job.error}${part}`);
        }
        if (job.state === AssistantJobState.DONE) {
          const made = job.suggestionIds.length;
          return this.c.toast(
            "info",
            made ? `Claude proposed changes for ${made === 1 ? (n === 1 ? "it" : "1 item") : `${made} items`} · S to review` : "Claude found nothing to change",
          );
        }
      }
    } catch {
      // The daemon went away: what it made, if anything, is in S.
    } finally {
      this.following.delete(id);
      this.show();
    }
  }
}
