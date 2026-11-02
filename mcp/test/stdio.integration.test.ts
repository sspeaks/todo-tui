// `todo mcp` end to end: the built binary execs the built bundle, which
// starts a real daemon in a scratch home and serves the tools on stdio.
// Skipped unless both are built (cargo build; pnpm -C mcp build).

import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { afterAll, describe, expect, it } from "vitest";

const root = resolve(import.meta.dirname, "../..");
const todo = process.env.TODO_BIN || join(root, "target/debug/todo");
const bundle = join(root, "mcp/dist/mcp.js");
const built = existsSync(todo) && existsSync(bundle);

describe.skipIf(!built)("todo mcp over stdio", () => {
  const home = mkdtempSync(join(tmpdir(), "todo-mcp-"));
  const env = { ...process.env, TODO_TUI_HOME: home, TODO_MCP_ENTRY: bundle, TODO_BIN: todo } as Record<string, string>;

  afterAll(() => {
    try {
      execFileSync(todo, ["daemon", "stop"], { env, stdio: "ignore" });
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  });

  it("captures, lists and completes through a real daemon", async () => {
    const client = new Client({ name: "test", version: "test" });
    await client.connect(new StdioClientTransport({ command: todo, args: ["mcp"], env }));
    try {
      const call = async (name: string, args: Record<string, unknown> = {}) => {
        const res = await client.callTool({ name, arguments: args });
        const text = (res.content as { text: string }[])[0]!.text;
        expect(res.isError, text).toBeFalsy();
        return JSON.parse(text);
      };
      const { item } = await call("todo_capture", { text: "Water the plants +3 #home @3d" });
      expect(item).toMatchObject({ title: "Water the plants", priority: 3, labels: ["home"], repeat: "@3d" });
      const listed = await call("todo_list", { view: "inbox" });
      expect(listed.items.map((i: { id: string }) => i.id)).toEqual([item.id]);
      const { next } = await call("todo_set_status", { id: item.short_id, status: "done" });
      expect(next).toMatchObject({ title: "Water the plants", repeat: "@3d" });
      const status = await call("todo_status");
      expect(status.inbox).toBe(0);
    } finally {
      await client.close();
    }
  }, 20_000);
});
