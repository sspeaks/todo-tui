// Prints every item in $TODO_TUI_HOME (done, archived, deferred and deleted
// too) as the MCP tools project them, for grading eval runs.
import { ClientKind, connect } from "@todo-tui/client/core";
import { project } from "../src/project.ts";

const conn = await connect({ clientKind: ClientKind.ONESHOT });
const res = await conn.items.listItems({
  filter: { includeArchived: true, includeDone: true, includeDeferred: true, includeDeleted: true },
  pageSize: 1000,
});
console.log(JSON.stringify(res.items.map((i) => project(i, { detail: true })), null, 2));
conn.close();
