// `todo mcp [--read-only]` (design 4.6): a stdio MCP server for Claude Code
// over the daemon, which it starts if nothing answers.

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { CLIENT_VERSION, ClientKind, connectAny } from "@todo-tui/client/core";
import { registerTools } from "./tools.js";

const args = process.argv.slice(2);
const unknown = args.filter((a) => a !== "--read-only");
if (unknown.length > 0) {
  process.stderr.write(`todo mcp: unexpected argument ${unknown[0]}\n`);
  process.exit(2);
}
const readOnly = args.includes("--read-only");

try {
  const conn = await connectAny({ clientKind: ClientKind.MCP });
  const server = new McpServer({ name: "todo", version: CLIENT_VERSION });
  registerTools(server, conn, { readOnly });
  const transport = new StdioServerTransport();
  transport.onclose = () => conn.close();
  await server.connect(transport);
} catch (err) {
  process.stderr.write(`todo mcp: ${err instanceof Error ? err.message : String(err)}\n`);
  process.exit(1);
}
