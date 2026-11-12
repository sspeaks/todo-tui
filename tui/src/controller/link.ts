// P and L (design 2.5, 5.7, 3.3): promote a local todo to a new Jira issue,
// or link it to an existing one by key. Both write to Jira and wait for it:
// the prompt says what will happen, enter does it, and a toast says what
// came of it. Neither is an undo entry (the issue it made or linked has no
// inverse).

import { ConnectError, type Item, type MutationResult, SyncService, createClient, errorReason } from "@todo-tui/client";
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
    if (mode.prompt === "link" && !/^[A-Za-z][A-Za-z0-9_]*-\d+$/.test(text)) {
      return this.c.setMode({ ...mode, error: "An issue key, e.g. PLAT-12" });
    }
    this.c.setMode(normalMode);
    const promote = mode.prompt === "promote";
    try {
      type Linked = { item?: Item; result?: MutationResult; suggestionId: string };
      // Sent once, never resent: a second promote is a second issue. Jira
      // may take a while to create, longer than an edit's wait.
      const res = await this.c.pipeline.once(
        promote ? "creating in Jira…" : "linking…",
        60_000,
        ({ timeoutMs }): Promise<Linked> =>
          promote ? this.client.promote({ itemId: id, project: text }, { timeoutMs }) : this.client.attach({ itemId: id, key: text }, { timeoutMs }),
        (r) => ({ items: r.item ? [r.item] : [], revision: r.result?.revision ?? 0n }),
      );
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
    case "CREATE_UNKNOWN":
    case "CREATED_UNREAD":
      // Jira may have made it: the daemon's words say where to look.
      return ConnectError.from(err).rawMessage;
    case undefined:
      // No answer: the daemon finishes it all the same, so look before P again.
      return promote ? `${what} yet: ${errorText(err)} · it may still be linked, look before P again` : `${what}: ${errorText(err)}`;
    default:
      // The daemon's answer (a Jira one may be UNAVAILABLE): its words.
      return `${what}: ${ConnectError.from(err).rawMessage}`;
  }
}
