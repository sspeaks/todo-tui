// Jira sync from the TUI (design 5.11, ADR-0006): `o` opens the selected
// Item's issue, `gs` and `:sync [instance]` start a pull, and `:sync
// status` says how each instance stands.
// Read-only: nothing here writes to Jira.

import { type Client, SyncService, SyncState, type SyncStatus, createClient, errorReason, isConnectionFailure, timestampDate } from "@todo-tui/client";
import { singleLine } from "../format/text.js";
import type { Controller } from "./controller.js";
import { type UrlOpener, systemOpener } from "./meeting.js";
import { DaemonUnavailable, errorText } from "./messages.js";

export type SyncClient = Client<typeof SyncService>;

/** "now", "2m", "3h", "4d": how long ago `at` was. */
export function ago(at: Date, now: Date): string {
  const s = Math.max(0, Math.floor((now.getTime() - at.getTime()) / 1000));
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86_400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86_400)}d`;
}

/** One instance's state in a word or two, as the status bar shows it. */
export function stateWord(status: SyncStatus, now: Date): string {
  switch (status.state) {
    case SyncState.SYNCING:
      return status.pulled ? `syncing… ${status.pulled}` : "syncing…";
    case SyncState.OFFLINE:
      return "offline";
    case SyncState.AUTH_ERROR:
      return "auth";
    case SyncState.DEGRADED:
      return "error";
    case SyncState.STARTING:
      return "starting";
    default: {
      const at = status.lastSuccessfulPullAt;
      return at ? ago(timestampDate(at), now) : "ok";
    }
  }
}

/** The status bar's colour for a state: red needs the user, yellow will retry. */
export function stateColor(state: SyncState): "red" | "yellow" | undefined {
  if (state === SyncState.AUTH_ERROR || state === SyncState.DEGRADED) return "red";
  if (state === SyncState.OFFLINE) return "yellow";
  return undefined;
}

/** The longer line :sync status toasts for one instance. */
export function statusLine(status: SyncStatus, now: Date): string {
  const at = status.lastSuccessfulPullAt;
  const last = at ? `last pull ${ago(timestampDate(at), now)}${ago(timestampDate(at), now) === "now" ? "" : " ago"}` : "not pulled yet";
  const word =
    status.state === SyncState.AUTH_ERROR
      ? "the token was refused: store a new one, then gs"
      : status.state === SyncState.OFFLINE
        ? "offline, retrying"
        : status.state === SyncState.DEGRADED
          ? "a pull failed"
          : status.state === SyncState.SYNCING
            ? "syncing"
            : status.state === SyncState.STARTING
              ? "starting"
              : "ok";
  const error = status.lastError && status.state !== SyncState.OK ? ` (${singleLine(status.lastError)})` : "";
  return `${singleLine(status.instanceId)}: ${word}${error} · ${status.items} item${status.items === 1 ? "" : "s"} · ${last}`;
}

export class Sync {
  private readonly client: SyncClient;

  constructor(
    private readonly c: Controller,
    private readonly open: UrlOpener = systemOpener,
    client?: SyncClient,
  ) {
    this.client = client ?? createClient(SyncService, c.store.transport);
  }

  /** o: the selected Item's issue in the browser. */
  async openRemote(): Promise<void> {
    const id = this.c.target(false)?.ids[0];
    const item = id === undefined ? undefined : this.c.store.getItem(id);
    if (!item) return;
    const url = item.remoteLink?.url;
    if (!url) return this.c.toast("info", "Not linked to Jira");
    try {
      await this.open(url);
    } catch (err) {
      return this.c.failed(`Could not open ${singleLine(item.remoteLink?.key ?? "it")} (${err instanceof Error ? err.message : String(err)}): ${url}`, { keepEnd: false });
    }
    this.c.toast("info", `Opened ${singleLine(item.remoteLink?.key ?? url)}`);
  }

  /** gs, :sync [instance]: a pull now; its progress is the status bar's. */
  async now(instance = ""): Promise<void> {
    try {
      await this.c.pipeline.call(({ timeoutMs }) => this.client.syncNow({ instanceId: instance }, timeoutMs !== undefined ? { timeoutMs } : {}));
    } catch (err) {
      if (errorReason(err) === "NO_INSTANCE") {
        return this.c.toast("info", instance ? `No instance ${JSON.stringify(instance)} is configured` : "Nothing to sync: no [[instances]] in the config");
      }
      if (err instanceof DaemonUnavailable || isConnectionFailure(err)) return this.c.failed("Not synced: daemon unavailable");
      return this.c.failed(`Not synced: ${errorText(err)}`);
    }
    this.c.toast("info", instance ? `Syncing ${instance}…` : "Syncing…");
  }

  /** :sync status: each instance's line. */
  status(): void {
    const all = [...this.c.store.getSyncStatus().values()];
    if (!all.length) return this.c.toast("info", "Nothing syncs: no [[instances]] in the config");
    const now = this.c.now();
    const bad = all.some((s) => stateColor(s.state) === "red");
    this.c.toast(bad ? "error" : "info", all.map((s) => statusLine(s, now)).join(" · "));
  }

  /** :sync [status | instance]. */
  run(args: string): Promise<void> | void {
    const word = args.trim();
    if (word === "status") return this.status();
    return this.now(word);
  }
}
