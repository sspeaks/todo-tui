// P and L (design 2.5, 5.7, 3.3): promote a local todo to a new Jira issue,
// or link it to an existing one by key. Both write to Jira and wait for it:
// the prompt says what will happen, enter does it, and a toast says what
// came of it. Neither is an undo entry (the issue it made or linked has no
// inverse).

import { type Item, type MutationResult, SyncService, createClient, errorReason } from "@todo-tui/client";
import { lineOf } from "../keys/line-editor.js";
import { type Mode, normalMode } from "../state/types.js";
import type { Controller } from "./controller.js";
import { errorText } from "./messages.js";

type Prompt = Extract<Mode, { kind: "prompt" }>;

export class Link {
  private readonly client;

  constructor(private readonly c: Controller) {
    this.client = createClient(SyncService, c.store.transport);
  }

  /** The selected local todo, or why it is none. */
  private local() {
    const target = this.c.target(false);
    const item = target && this.c.store.getItem(target.ids[0] ?? "");
    if (!target || !item) return undefined;
    if (item.remoteLink) {
      this.c.toast("info", `Already ${item.remoteLink.key}: o opens it`);
      return undefined;
    }
    return { target, item };
  }

  /** P: a prompt for the project, prefilled with promote_default's. */
  promote(project?: string): void {
    const found = this.local();
    if (!found) return;
    const instance = this.c.store.getConfig().config?.instances[0];
    if (!instance) return this.c.toast("error", "No Jira: add [[instances]] to the config");
    const text = project?.trim() || instance.promoteProject || "";
    this.c.setMode({ kind: "prompt", prompt: "promote", target: found.target, line: lineOf(text) });
  }

  /** L: a prompt for the issue's key. */
  attach(key?: string): void {
    const found = this.local();
    if (!found) return;
    if (!this.c.store.getConfig().config?.instances[0]) return this.c.toast("error", "No Jira: add [[instances]] to the config");
    this.c.setMode({ kind: "prompt", prompt: "link", target: found.target, line: lineOf(key?.trim().toUpperCase() ?? "") });
  }

  /** enter: creates the issue, or links it, and says so. */
  async submit(mode: Prompt): Promise<void> {
    const id = mode.target.ids[0] ?? "";
    const text = mode.line.text.trim();
    if (mode.prompt === "link" && !/^[A-Za-z][A-Za-z0-9]*-\d+$/.test(text)) {
      return this.c.setMode({ ...mode, error: "An issue key, e.g. PLAT-12" });
    }
    this.c.setMode(normalMode);
    const promote = mode.prompt === "promote";
    this.c.toast("info", promote ? "Creating it in Jira…" : `Linking it to ${text.toUpperCase()}…`);
    try {
      // Jira may take a while to create: longer than an edit's wait.
      const opts = { timeoutMs: 60_000 };
      type Linked = { item?: Item; result?: MutationResult; suggestionId: string };
      const res = await this.c.pipeline.call(
        (): Promise<Linked> => (promote ? this.client.promote({ itemId: id, project: text }, opts) : this.client.attach({ itemId: id, key: text }, opts)),
      );
      if (res.item) this.c.store.applyItems([res.item], res.result?.revision ?? 0n);
      const key = res.item?.remoteLink?.key ?? text.toUpperCase();
      const differ = res.suggestionId ? " · what Jira changed of yours is in S" : "";
      this.c.toast("info", `${promote ? "Created" : "Linked to"} ${key}${differ}`);
    } catch (err) {
      this.c.failed(linkError(err, promote));
    }
  }
}

/** A refusal in words the person can act on. */
function linkError(err: unknown, promote: boolean): string {
  const what = promote ? "Not created" : "Not linked";
  switch (errorReason(err)) {
    case "NO_PROMOTE_TARGET":
      return `${what}: give a project, or set promote_default = { project = "…", issue_type = "Task" } in [[instances]]`;
    case "PUSH_OFF":
      return `${what}: Jira is read-only here (push = false)`;
    case "MISSING_FIELDS":
    case "ALREADY_LINKED":
    case "ISSUE_NOT_FOUND":
    case "JIRA_UNAVAILABLE":
    case "JIRA_REFUSED":
      return `${what}: ${errorText(err)}`;
    default:
      return `${what}: ${errorText(err)}`;
  }
}
