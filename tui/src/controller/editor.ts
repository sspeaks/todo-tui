// E (Decision 10): the selected Item's description in $VISUAL / $EDITOR.
// Ink hands the terminal over (useApp().suspendTerminal) around a spawnSync
// of the editor, which, unlike an async spawn, leaves no typed key for the
// TUI to read (m4_code §5.5); the change stream catches up on resume. A
// non-zero exit saves nothing, one trailing newline does not count as a
// change, and unchanged text sends nothing. The save carries
// expected_revision: a conflict rebases silently when the description is
// unchanged on the server; otherwise, or when the save fails, the text goes
// to <state dir>/recovered/ so that it is never lost (Decision 6).

import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { type Item, errorReason, updateItem } from "@todo-tui/client";
import type { Controller } from "./controller.js";
import { withoutInterrupts } from "./job.js";
import { errorText, opText, quoted } from "./messages.js";
import { single } from "./pipeline.js";

/** What the editor left: the file's text, or why there is none. */
export type EditorOutcome = { readonly ok: true; readonly text: string } | { readonly ok: false; readonly reason: string };

/** Runs an editor on `text` and reads back what it saved; injectable, so tests need no terminal. */
export type EditorRunner = (text: string) => Promise<EditorOutcome>;

/** Hands the terminal to `run` and takes it back afterwards (Ink's suspendTerminal). */
export type Suspend = (run: () => void | Promise<void>) => Promise<void>;

/** How many times a save rebases silently before it keeps the text aside. */
const MAX_REBASES = 3;

/** The text without the one newline an editor adds at the end. */
export const withoutFinalNewline = (text: string): string => (text.endsWith("\n") ? text.slice(0, -1) : text);

/**
 * The system editor: $VISUAL, else $EDITOR, else vi, run through sh as git
 * runs it (the command is part of the script, so "code --wait" and quoted
 * arguments work), on a temp .md file that is removed afterwards. The
 * terminal is handed over through `suspend()` when the shell set one.
 */
export function systemEditor(suspend: () => Suspend | undefined, env: NodeJS.ProcessEnv = process.env): EditorRunner {
  return async (text) => {
    const dir = await mkdtemp(join(tmpdir(), "todo-edit-"));
    const file = join(dir, "description.md");
    try {
      await writeFile(file, text);
      const editor = env.VISUAL || env.EDITOR || "vi";
      let result: ReturnType<typeof spawnSync> | undefined;
      // A ctrl-c given to the editor must not end the TUI (job.ts).
      const run = () =>
        withoutInterrupts(() => {
          result = spawnSync("sh", ["-c", `${editor} "$@"`, editor, file], { stdio: "inherit" });
        });
      const handOver = suspend();
      if (handOver) await handOver(run);
      else await run();
      if (!result || result.error) return { ok: false, reason: `Could not run ${editor}, not saved` };
      if (result.signal) return { ok: false, reason: `Editor stopped by ${result.signal}, not saved` };
      // sh's own statuses: the command was not found, or not executable.
      if (result.status === 127) return { ok: false, reason: `Could not run the editor ${editor} (not found; set $VISUAL or $EDITOR), not saved` };
      if (result.status === 126) return { ok: false, reason: `Could not run the editor ${editor} (not executable), not saved` };
      if (result.status !== 0) return { ok: false, reason: `Editor exited ${result.status}, not saved` };
      return { ok: true, text: await readFile(file, "utf8") };
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  };
}

/** A path under the home directory as ~/…, shorter in a toast. */
export function homeShort(path: string, home: string = homedir()): string {
  return home && home !== "/" && path.startsWith(`${home}/`) ? `~${path.slice(home.length)}` : path;
}

/** Writes `text` to <stateDir>/recovered/<id>-<timestamp>.md and returns the path. */
export async function saveRecovered(stateDir: string, id: string, text: string, at: Date): Promise<string> {
  const dir = join(stateDir, "recovered");
  await mkdir(dir, { recursive: true, mode: 0o700 });
  const stamp = at.toISOString().replace(/[-:]/g, "").replace(/\.\d+Z$/, "Z");
  const path = join(dir, `${id}-${stamp}.md`);
  await writeFile(path, text.endsWith("\n") ? text : `${text}\n`, { mode: 0o600 });
  return path;
}

export class DescriptionEditor {
  constructor(
    private readonly c: Controller,
    private readonly runner: EditorRunner,
    /** The state dir, for the recovered texts (resolved only when one is needed). */
    private readonly stateDir: () => string,
  ) {}

  /** E: edits the selected Item's description (never the marked ones). */
  async edit(): Promise<void> {
    const item = this.c.selectedItem();
    if (!item) return;
    const base = { value: item.description, revision: item.revision };
    const before = withoutFinalNewline(base.value);
    // A final newline, as editors keep a file.
    const out = await this.runner(before ? `${before}\n` : "");
    if (!out.ok) return this.c.toast("error", out.reason);
    const text = withoutFinalNewline(out.text);
    if (text === before) return this.c.toast("info", "Description unchanged");
    return this.save(item, text, base);
  }

  private async save(item: Item, text: string, base: { value: string; revision: bigint }): Promise<void> {
    let expectedRevision = base.revision;
    for (let rebases = 0; ; rebases++) {
      const revision = expectedRevision;
      const out = await this.c.pipeline.run({
        call: (opts) => updateItem(this.c.items, { id: item.id, description: text }, ["description"], { ...opts, expectedRevision: revision }),
        items: single,
      });
      if (out.ok) return this.c.toastOp(out.res.result?.opId, opText("edit", item.title));
      if (errorReason(out.err) !== "REVISION_MISMATCH") return this.keep(item, text, errorText(out.err, item.title));
      let fresh: Item | undefined;
      try {
        fresh = await this.c.store.fetchItem(item.id);
      } catch (err) {
        return this.keep(item, text, errorText(err, item.title));
      }
      if (!fresh || fresh.deletedAt) return this.keep(item, text, `${quoted(item.title)} was deleted elsewhere`);
      // Only other fields changed: save on the new revision.
      if (fresh.description === base.value && rebases < MAX_REBASES) {
        expectedRevision = fresh.revision;
        continue;
      }
      return this.keep(item, text, "Description changed elsewhere");
    }
  }

  /**
   * A save that did not happen: the text goes to the recovered dir, and the
   * toast says where, cut in the middle when it does not fit, so that the
   * file's name always shows.
   */
  private async keep(item: Item, text: string, why: string): Promise<void> {
    let path: string;
    try {
      path = await saveRecovered(this.stateDir(), item.id, text, new Date());
    } catch (err) {
      return this.c.failed(`${why}; your text could not be kept either: ${err instanceof Error ? err.message : String(err)}`);
    }
    this.c.failed(`${why}; your text is in ${homeShort(path)}`, { keepEnd: true });
  }
}
