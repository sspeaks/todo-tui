// `:export [path]` (design 7.4): every record of the database through
// SystemService.Export, written to `path` (~ for home), by default
// ~/todo-export-YYYYMMDD-HHMM.jsonl. Written beside it and renamed into
// place, so a file of that name is always whole.

import { renameSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { type Client, createClient, SystemService } from "@todo-tui/client";
import type { Controller } from "./controller.js";
import { homeShort } from "./editor.js";
import { errorText } from "./messages.js";

/** Writes a file as the export needs it (tests replace it). */
export interface ExportFs {
  writeFileSync(path: string, text: string): void;
  renameSync(from: string, to: string): void;
}

const nodeFs: ExportFs = { writeFileSync, renameSync };

const pad = (n: number) => String(n).padStart(2, "0");

/** ~/todo-export-YYYYMMDD-HHMM.jsonl, local time. */
export function defaultExportPath(now: Date, home = homedir()): string {
  const stamp = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}`;
  return join(home, `todo-export-${stamp}.jsonl`);
}

export class Exporter {
  private readonly system: Client<typeof SystemService>;

  constructor(
    private readonly c: Controller,
    private readonly fs: ExportFs = nodeFs,
    private readonly now: () => Date = () => new Date(),
  ) {
    this.system = createClient(SystemService, c.store.transport);
  }

  async run(args: string): Promise<void> {
    const typed = args.trim();
    const path = typed ? typed.replace(/^~(?=$|\/)/, homedir()) : defaultExportPath(this.now());
    let lines: string[] = [];
    try {
      lines = await this.c.pipeline.call(async ({ timeoutMs }) => {
        const out: string[] = [];
        for await (const batch of this.system.export({}, { ...(timeoutMs !== undefined && { timeoutMs }) })) out.push(...batch.lines);
        return out;
      });
    } catch (err) {
      return this.c.failed(`Not exported: ${errorText(err)}`);
    }
    const items = lines.filter((l) => {
      try {
        return (JSON.parse(l) as { type?: unknown }).type === "item";
      } catch {
        return false;
      }
    }).length;
    try {
      const tmp = `${path}.tmp`;
      this.fs.writeFileSync(tmp, lines.map((l) => `${l}\n`).join(""));
      this.fs.renameSync(tmp, path);
    } catch (err) {
      return this.c.failed(`Not exported: ${err instanceof Error ? err.message : String(err)}`);
    }
    this.c.toast("info", `Exported ${items} item${items === 1 ? "" : "s"} to ${homeShort(path)}`, { keepEnd: true });
  }
}
