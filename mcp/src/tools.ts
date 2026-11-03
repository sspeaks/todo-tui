// The MCP tools (design 4.6): a curated set, each one or two ItemService or
// ViewService calls through @todo-tui/client. No logic of its own beyond
// resolving ids and dates; the daemon validates everything.

import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import {
  Code,
  ConnectError,
  CreatedVia,
  type FilterSchema,
  type Item,
  ItemField,
  type MessageInitShape,
  MAX_MATCHES_BYTES,
  Operator,
  Priority,
  type ProtoDate,
  type TodoClients,
  archiveItem,
  captureItem,
  deleteItem,
  errorReason,
  getStatusCounts,
  listViews,
  parseDate,
  parseRecurrence,
  redo,
  setRecurrence,
  setStatus,
  unarchiveItem,
  undeleteItem,
  undo,
  updateItem,
} from "@todo-tui/client/core";
import { z } from "zod";
import { PRIORITY_LEVELS, STATUS_NAMES, isoDate, project, shortId } from "./project.js";

export interface ToolOptions {
  /** Register only the tools that read (`todo mcp --read-only`). */
  readOnly?: boolean;
}

type FilterInit = MessageInitShape<typeof FilterSchema>;
type Result = { content: { type: "text"; text: string }[] };

const json = (value: unknown): Result => ({ content: [{ type: "text", text: JSON.stringify(value, null, 2) }] });

/** A tool error a model can act on; the SDK turns a throw into an isError result. */
class ToolError extends Error {}

/** The ConnectError as one line, with the ErrorInfo reason when there is one. */
function describe(err: unknown): Error {
  if (err instanceof ToolError) return err;
  if (err instanceof ConnectError) {
    const reason = errorReason(err);
    if (err.code === Code.Aborted) return new ToolError(`changed elsewhere since it was read; get it again and retry (${err.rawMessage})`);
    if (err.code === Code.Unavailable) return new ToolError(`the todo daemon is not answering: ${err.rawMessage}`);
    return new ToolError(`${Code[err.code]}${reason ? ` ${reason}` : ""}: ${err.rawMessage}`);
  }
  return err instanceof Error ? err : new Error(String(err));
}

const ID = z
  .string()
  .min(1)
  .describe("The item's id: the full 26-character id, or the 8-character short id that `todo add` and every tool print");

const DATE = z
  .string()
  .describe('A date as capture takes it: "today", "tom", "fri", "+3d", "+2w", "eow", "eom", "nov2" or "2026-11-02". Empty string clears it.');

const STATUS = z.enum(Object.keys(STATUS_NAMES) as [string, ...string[]]);

const textOf = (text: string): FilterInit["anyOf"] =>
  [ItemField.TITLE, ItemField.LABELS, ItemField.REMOTE_LINK_KEY, ItemField.DESCRIPTION].map((target) => ({
    kind: { case: "predicate", value: { target, op: Operator.MATCHES, operands: [{ kind: { case: "text", value: text } }] } },
  }));

export function registerTools(server: McpServer, clients: TodoClients, opts: ToolOptions = {}): void {
  const { items, views } = clients;

  /** Runs `fn`, rewording daemon errors for a model. */
  async function run(fn: () => Promise<unknown>): Promise<Result> {
    try {
      return json(await fn());
    } catch (err) {
      throw describe(err);
    }
  }

  /** The Item a full or short id names; a short id must name exactly one. */
  async function resolve(id: string): Promise<Item> {
    const wanted = id.trim().toUpperCase();
    if (wanted.length === 26) {
      const res = await items.getItem({ id: wanted });
      if (!res.item) throw new ToolError(`no item ${id}`);
      return res.item;
    }
    if (!/^[0-9A-HJKMNP-TV-Z]{4,25}$/.test(wanted)) throw new ToolError(`${id} is not an item id`);
    const found: Item[] = [];
    let pageToken = "";
    do {
      const page = await items.listItems({
        filter: { includeArchived: true, includeDone: true, includeDeferred: true },
        pageSize: 1000,
        pageToken,
      });
      found.push(...page.items.filter((i) => i.id.endsWith(wanted)));
      pageToken = page.nextPageToken;
    } while (pageToken !== "");
    if (found.length === 0) throw new ToolError(`no item has the id ${id}`);
    if (found.length > 1) {
      throw new ToolError(`${id} names ${found.length} items (${found.map((i) => `${shortId(i.id)} "${i.title}"`).join(", ")}); use the full id`);
    }
    return found[0]!;
  }

  /** A capture-grammar date, or undefined for "" (clear). */
  async function date(text: string): Promise<ProtoDate | undefined> {
    if (text.trim() === "") return undefined;
    const res = await parseDate(items, text);
    if (!res.date) throw new ToolError(`${text}: ${res.warning || "not a date"}`);
    return res.date;
  }

  /**
   * The id of the view named `name` (case-insensitive), else of the default
   * seeded as it ("today"), else with that id: a name wins, since views can
   * be renamed and a user's view can take a default's old name.
   */
  async function viewId(name: string): Promise<string> {
    const { views: all } = await listViews(views);
    const key = name.trim().toLowerCase();
    const view = all.find((v) => v.name.toLowerCase() === key) ?? all.find((v) => v.seed === key) ?? all.find((v) => v.id === name);
    if (!view) throw new ToolError(`no view named ${name}; the views are ${all.map((v) => v.name).join(", ")}`);
    return view.id;
  }

  // Reads.

  server.registerTool(
    "todo_list",
    {
      title: "List todos",
      description:
        "List items, optionally within a saved view (Inbox, Today, Next, Waiting, Someday, Archive) and narrowed by text or labels. " +
        "Without a view it lists every open item. Done, cancelled, archived and deferred items are left out unless include_done is set " +
        "(a view's own filter may include them). Text matches the title, labels, description and Jira key.",
      inputSchema: {
        view: z.string().optional().describe('A view name, e.g. "today" or "waiting"'),
        text: z.string().max(MAX_MATCHES_BYTES).optional().describe("Words to match"),
        labels: z.array(z.string()).optional().describe("Only items with any of these labels (exact, without #)"),
        include_done: z.boolean().optional().describe("Also done, cancelled, archived and deferred items"),
        limit: z.number().int().min(1).max(500).optional().describe("At most this many (default 100)"),
      },
      annotations: { readOnlyHint: true },
    },
    ({ view, text, labels, include_done, limit }) =>
      run(async () => {
        const filter: FilterInit = {
          ...(text?.trim() && { anyOf: textOf(text.trim()) }),
          ...(labels?.length && {
            allOf: [
              {
                kind: {
                  case: "predicate",
                  value: {
                    target: ItemField.LABELS,
                    op: Operator.CONTAINS_ANY,
                    operands: labels.map((l) => ({ kind: { case: "value", value: { kind: { case: "stringValue", value: l.replace(/^#/, "") } } } })),
                  },
                },
              },
            ],
          }),
          ...(include_done && { includeDone: true, includeArchived: true, includeDeferred: true }),
        };
        const res = await items.listItems({
          filter,
          pageSize: limit ?? 100,
          ...(view !== undefined && { viewId: await viewId(view) }),
        });
        return { items: res.items.map((i) => project(i)), more: res.nextPageToken !== "" };
      }),
  );

  server.registerTool(
    "todo_get",
    {
      title: "Get a todo",
      description: "One item in full, including its description and private notes.",
      inputSchema: { id: ID },
      annotations: { readOnlyHint: true },
    },
    ({ id }) => run(async () => project(await resolve(id), { detail: true })),
  );

  server.registerTool(
    "todo_status",
    {
      title: "Todo counts",
      description: "The inbox, due and overdue counts, the count of each view that shows one, and today's date as the daemon sees it.",
      annotations: { readOnlyHint: true },
    },
    () =>
      run(async () => {
        const [counts, { views: all }] = await Promise.all([getStatusCounts(items, { includeViews: true }), listViews(views)]);
        const names = new Map(all.map((v) => [v.id, v.name]));
        return {
          today: isoDate(counts.today),
          inbox: counts.counts?.inbox ?? 0,
          due: counts.counts?.due ?? 0,
          overdue: counts.counts?.overdue ?? 0,
          views: Object.fromEntries(counts.views.map((v) => [names.get(v.viewId) ?? v.viewId, v.count])),
        };
      }),
  );

  server.registerTool(
    "todo_views",
    {
      title: "List views",
      description: "The saved views (tabs) in order, with their names.",
      annotations: { readOnlyHint: true },
    },
    () =>
      run(async () => {
        const { views: all } = await listViews(views);
        return all.map((v) => ({ name: v.name, ...(v.seed !== undefined && { seed: v.seed }) }));
      }),
  );

  if (opts.readOnly) return;

  // Writes.

  server.registerTool(
    "todo_capture",
    {
      title: "Capture a todo",
      description:
        "Create an item from capture text, exactly as `todo add` does. Fields go in inline tokens: +1..+5 priority (1 highest), " +
        "^date due, #label, ~person or ~\"two words\" waiting on, >next or >someday status (default inbox), @rule repeat " +
        "(@daily, @weekdays, @mon,thu, @weekly, @monthly, @15th, @3d, @2w), \\ escapes a sigil. A token that does not parse stays in the title and comes back in warnings.",
      inputSchema: {
        text: z.string().min(1).describe('e.g. "Send Priya the Q4 deck +2 ^fri #work"'),
        parent: ID.optional().describe("Capture it as a subtask of this item"),
      },
    },
    ({ text, parent }) =>
      run(async () => {
        const parentId = parent === undefined ? undefined : (await resolve(parent)).id;
        const res = await captureItem(items, text, { createdVia: CreatedVia.MCP, ...(parentId !== undefined && { parentId }) });
        return { item: res.item && project(res.item), warnings: res.draft?.warnings.length ? res.draft.warnings : undefined };
      }),
  );

  server.registerTool(
    "todo_update",
    {
      title: "Update a todo",
      description:
        "Change fields of an item; only the fields given change. Empty string (or null) clears a field. " +
        "Status, repeat, archive and delete have their own tools. On a Jira-linked item, prefer the local fields " +
        "(defer_until, waiting, private_notes, local_priority): the rest are Jira's and are pushed there.",
      inputSchema: {
        id: ID,
        title: z.string().optional(),
        description: z.string().optional().describe("Markdown"),
        priority: z.number().int().min(0).max(5).optional().describe("1 (highest) to 5 (lowest); 0 for none"),
        local_priority: z.number().int().min(0).max(5).optional().describe("A local override of priority, never pushed to Jira; 0 clears it"),
        due: DATE.optional(),
        defer_until: DATE.optional().describe("Hide the item from views until this date"),
        labels: z.array(z.string()).optional().describe("The whole new label set (replaces the old one)"),
        waiting_on: z.string().nullable().optional().describe("Who or what it waits on; null or empty to stop waiting"),
        waiting_reason: z.string().optional(),
        follow_up: DATE.optional().describe("When to chase the waiting-on"),
        private_notes: z.string().optional().describe("Markdown, local only"),
      },
    },
    (args) =>
      run(async () => {
        const item = await resolve(args.id);
        const patch: Record<string, unknown> = { id: item.id };
        const paths: string[] = [];
        const set = (path: string, value: unknown, key = path) => {
          paths.push(path);
          if (value !== undefined) patch[key] = value;
        };
        if (args.title !== undefined) set("title", args.title);
        if (args.description !== undefined) set("description", args.description);
        if (args.priority !== undefined) set("priority", args.priority === 0 ? Priority.NONE : PRIORITY_LEVELS[args.priority]);
        if (args.local_priority !== undefined) {
          set("local_priority_override", args.local_priority === 0 ? undefined : PRIORITY_LEVELS[args.local_priority], "localPriorityOverride");
        }
        if (args.due !== undefined) set("due_date", await date(args.due), "dueDate");
        if (args.defer_until !== undefined) set("defer_until", await date(args.defer_until), "deferUntil");
        if (args.labels !== undefined) set("labels", args.labels.map((l) => l.replace(/^#/, "")));
        if (args.private_notes !== undefined) set("private_notes", args.private_notes, "privateNotes");
        if (args.waiting_on === null || args.waiting_on === "") {
          set("waiting", undefined);
        } else if (args.waiting_on !== undefined || args.waiting_reason !== undefined || args.follow_up !== undefined) {
          // Start waiting with the whole message (since defaults to today);
          // on an item already waiting, change only the subfields given.
          const followUp = args.follow_up === undefined ? undefined : await date(args.follow_up);
          const sub: [string, string, unknown][] = [];
          if (args.waiting_on !== undefined) sub.push(["waiting.on", "on", { ref: { case: "freeText", value: args.waiting_on } }]);
          if (args.waiting_reason !== undefined) sub.push(["waiting.reason", "reason", args.waiting_reason]);
          if (args.follow_up !== undefined) sub.push(["waiting.follow_up_on", "followUpOn", followUp]);
          if (!item.waiting && args.waiting_on === undefined) throw new ToolError("the item is not waiting; give waiting_on to start");
          const waiting: Record<string, unknown> = {};
          for (const [, key, value] of sub) if (value !== undefined) waiting[key] = value;
          patch.waiting = waiting;
          if (item.waiting) paths.push(...sub.map(([path]) => path));
          else paths.push("waiting");
        }
        if (paths.length === 0) throw new ToolError("give at least one field to change");
        const res = await updateItem(items, patch as Parameters<typeof updateItem>[1], paths);
        return res.item && project(res.item);
      }),
  );

  server.registerTool(
    "todo_set_status",
    {
      title: "Set a todo's status",
      description:
        "Move an item to inbox, next, someday or in_progress, or close it as done or cancelled. Completing (or cancelling) " +
        "a repeating item creates its next instance, returned as `next`.",
      inputSchema: { id: ID, status: STATUS },
    },
    ({ id, status }) =>
      run(async () => {
        const item = await resolve(id);
        const res = await setStatus(items, item.id, STATUS_NAMES[status]!);
        return { item: res.item && project(res.item), next: res.generated && project(res.generated) };
      }),
  );

  server.registerTool(
    "todo_set_repeat",
    {
      title: "Set a todo's repeat rule",
      description:
        "Set, replace or clear (empty string) an open item's repeat rule: daily, weekdays, mon,thu, weekly, monthly, 15th, 3d or 2w " +
        "(an @ is optional). weekly and monthly take the weekday or day from the item's due date. Without a due date, the first scheduled date becomes it.",
      inputSchema: { id: ID, rule: z.string() },
    },
    ({ id, rule }) =>
      run(async () => {
        const item = await resolve(id);
        if (rule.trim() === "") {
          const res = await setRecurrence(items, item.id, undefined);
          return res.item && project(res.item);
        }
        const parsed = await parseRecurrence(items, rule, item.dueDate ? { anchor: item.dueDate } : {});
        if (!parsed.rule) throw new ToolError(`${rule}: ${parsed.warning || "not a repeat rule"}`);
        const res = await setRecurrence(items, item.id, parsed.rule);
        return res.item && project(res.item);
      }),
  );

  const simple = (
    name: string,
    title: string,
    description: string,
    call: (id: string) => Promise<{ item?: Item | undefined }>,
    destructive = false,
  ) =>
    server.registerTool(
      name,
      { title, description, inputSchema: { id: ID }, annotations: { destructiveHint: destructive } },
      ({ id }) =>
        run(async () => {
          const res = await call((await resolve(id)).id);
          return res.item && project(res.item);
        }),
    );

  simple("todo_archive", "Archive a todo", "Archive an item: it leaves every view but the Archive one.", (id) => archiveItem(items, id));
  simple("todo_unarchive", "Unarchive a todo", "Bring an archived item back.", (id) => unarchiveItem(items, id));
  simple(
    "todo_delete",
    "Delete a todo",
    "Soft-delete an item and its subtasks (todo_undelete or todo_undo bring it back). Deleting the open instance of a repeating item ends the series. Never deletes anything in Jira.",
    (id) => deleteItem(items, id),
    true,
  );
  simple("todo_undelete", "Undelete a todo", "Bring a deleted item back.", (id) => undeleteItem(items, id));

  server.registerTool(
    "todo_undo",
    {
      title: "Undo",
      description: "Undo the most recent change, whoever made it (this tool, the TUI or `todo add`).",
    },
    () =>
      run(async () => {
        const res = await undo(items);
        return {
          undid: res.entry?.description ?? "nothing to undo",
          items: res.items.map((i) => project(i)),
          skipped: res.skipped.length ? res.skipped.map((s) => s.message) : undefined,
        };
      }),
  );

  server.registerTool(
    "todo_redo",
    { title: "Redo", description: "Redo the most recently undone change." },
    () =>
      run(async () => {
        const res = await redo(items);
        return {
          redid: res.entry?.description ?? "nothing to redo",
          items: res.items.map((i) => project(i)),
          skipped: res.skipped.length ? res.skipped.map((s) => s.message) : undefined,
        };
      }),
  );
}
