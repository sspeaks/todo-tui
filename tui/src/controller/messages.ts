// What the message line says after an action (Decision 5 and 6, m4_arch
// §3.2): errors mapped to a sentence, and the undo and redo results in
// the CLI's words (crates/todo-cli/src/undo.rs).

import { BadRequestSchema, Code, ConnectError, type RedoResponse, type UndoResponse, errorReason, isConnectionFailure } from "@todo-tui/client";
import { singleLine, truncate } from "../format/text.js";

/** An action gave up waiting for the daemon after a connection failure. */
export class DaemonUnavailable extends Error {
  constructor(cause: unknown) {
    super("daemon unavailable", { cause });
  }
}

/** A title in a message, as OpSummary cuts it: at most 40 characters. */
export const quoted = (title: string): string => `“${truncate(singleLine(title), 40)}”`;

/** What an op did, as OpSummary words it: `mark done "x"`. */
export const opText = (verb: string, title: string): string => `${verb} "${truncate(singleLine(title), 40)}"`;

/** One line for an action's error; `title` names the Item it acted on. */
export function errorText(err: unknown, title?: string): string {
  if (err instanceof DaemonUnavailable) return "Not saved: daemon unavailable";
  switch (errorReason(err)) {
    case "ITEM_DELETED":
    case "ITEM_NOT_FOUND":
      return title === undefined ? "The Item was deleted elsewhere" : `${quoted(title)} was deleted elsewhere`;
    case "PARENT_DELETED":
      return "Undelete the parent first";
  }
  if (isConnectionFailure(err)) return "Not saved: daemon unavailable";
  const e = ConnectError.from(err);
  if (e.code === Code.DeadlineExceeded) return "The daemon did not answer; the change may not be saved";
  const violation = e.findDetails(BadRequestSchema)[0]?.fieldViolations[0];
  if (e.code === Code.InvalidArgument && violation) {
    return `Invalid ${violation.field.replace(/^item\./, "")}: ${singleLine(violation.description)}`;
  }
  return `${Code[e.code]}: ${singleLine(e.rawMessage)}`;
}

/** `Undid: …` / `Could not undo: …` / `Nothing to undo`, plus the changes it left alone. */
export function undoText(res: UndoResponse | RedoResponse, redo: boolean): string {
  const [done, failed, nothing] = redo ? ["Redid", "Could not redo", "Nothing to redo"] : ["Undid", "Could not undo", "Nothing to undo"];
  const { entry } = res;
  if (!entry) return nothing;
  const description = singleLine(entry.description);
  if (!res.items.length) return `${failed}: ${description} (everything it changed has changed since)`;
  const n = res.skipped.length;
  return `${done}: ${description}${n ? ` · ${n} change${n === 1 ? "" : "s"} kept (changed since)` : ""}`;
}
