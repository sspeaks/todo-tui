import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { FakeDaemon, ItemField, LinkState, Priority, Status, createClients } from "@todo-tui/client/core";
import { describe, expect, it } from "vitest";
import { registerTools } from "../src/tools.js";

// Monday 2026-11-02, noon UTC.
const NOW = new Date("2026-11-02T12:00:00Z");

async function harness(opts: { readOnly?: boolean; items?: ConstructorParameters<typeof FakeDaemon>[0] } = {}) {
  const daemon = new FakeDaemon(opts.items ?? [], { now: () => NOW, timeZone: "UTC" });
  const server = new McpServer({ name: "todo", version: "test" });
  registerTools(server, createClients(daemon.transport), { readOnly: opts.readOnly ?? false });
  const [a, b] = InMemoryTransport.createLinkedPair();
  const client = new Client({ name: "test", version: "test" });
  await Promise.all([server.connect(a), client.connect(b)]);
  /** Calls `name`; resolves to the parsed JSON, or rejects with the tool error text. */
  const call = async (name: string, args: Record<string, unknown> = {}): Promise<any> => {
    const res = await client.callTool({ name, arguments: args });
    const text = (res.content as { text: string }[])[0]!.text;
    if (res.isError) throw new Error(text);
    return JSON.parse(text);
  };
  return { daemon, client, call };
}

describe("todo mcp tools", () => {
  it("captures with tokens and lists in a view", async () => {
    const { call } = await harness();
    const { item, warnings } = await call("todo_capture", { text: "Send Priya the deck +2 ^tom #work >next" });
    expect(warnings).toBeUndefined();
    expect(item).toMatchObject({ title: "Send Priya the deck", status: "next", priority: 2, due: "2026-11-03", labels: ["work"] });
    expect(item.short_id).toBe(item.id.slice(-8));

    await call("todo_capture", { text: "Buy milk" });
    const next = await call("todo_list", { view: "next" });
    expect(next.items.map((i: { title: string }) => i.title)).toEqual(["Send Priya the deck"]);
    const all = await call("todo_list", { text: "milk" });
    expect(all.items.map((i: { title: string }) => i.title)).toEqual(["Buy milk"]);
    const tagged = await call("todo_list", { labels: ["#work"] });
    expect(tagged.items).toHaveLength(1);
  });

  it("finds a view by name before a renamed default's seed", async () => {
    const { daemon, call } = await harness();
    const views = createClients(daemon.transport).views;
    const today = (await views.listViews({})).views.find((v) => v.seed === "today")!;
    await views.updateView({ requestId: "01J00000000000000000000001", view: { id: today.id, name: "Focus" }, updateMask: { paths: ["name"] } });
    await views.createView({ requestId: "01J00000000000000000000002", name: "Today", filter: {}, sort: [], showCount: true });
    await call("todo_capture", { text: "open one >next" });
    const listed = await call("todo_list", { view: "Today" });
    expect(listed.items.map((i: { title: string }) => i.title)).toEqual(["open one"]);
    // The seed still finds the renamed default.
    await call("todo_list", { view: "today" });
  });

  it("reports parse warnings and an unknown view", async () => {
    const { call } = await harness();
    const { item, warnings } = await call("todo_capture", { text: "Call home @home" });
    expect(item.title).toBe("Call home @home");
    expect(warnings).toEqual([expect.stringContaining("@home")]);
    await expect(call("todo_list", { view: "nope" })).rejects.toThrow(/no view named nope; the views are Inbox/);
  });

  it("resolves short ids, and refuses unknown ones", async () => {
    const { call } = await harness();
    const { item } = await call("todo_capture", { text: "Dentist" });
    const got = await call("todo_get", { id: item.short_id.toLowerCase() });
    expect(got.id).toBe(item.id);
    await expect(call("todo_get", { id: "ZZZZZZZZ" })).rejects.toThrow("no item has the id ZZZZZZZZ");
    await expect(call("todo_get", { id: "not-an-id" })).rejects.toThrow("not-an-id is not an item id");
  });

  it("updates fields, parsing dates and clearing with an empty string", async () => {
    const { call } = await harness();
    const { item } = await call("todo_capture", { text: "Review RFC ^fri" });
    const updated = await call("todo_update", {
      id: item.short_id,
      title: "Review the RFC",
      priority: 1,
      labels: ["#work", "rfc"],
      defer_until: "+2d",
      private_notes: "ask about caching",
    });
    expect(updated).toMatchObject({ title: "Review the RFC", priority: 1, labels: ["work", "rfc"], due: "2026-11-06", defer_until: "2026-11-04" });
    const overridden = await call("todo_update", { id: item.id, local_priority: 4 });
    expect(overridden).toMatchObject({ priority: 4, own_priority: 1 });
    expect((await call("todo_update", { id: item.id, local_priority: 0 })).own_priority).toBeUndefined();
    const cleared = await call("todo_update", { id: item.id, due: "", priority: 0 });
    expect(cleared.due).toBeUndefined();
    expect(cleared.priority).toBeUndefined();
    expect((await call("todo_update", { id: item.id, energy: "low" })).energy).toBe("low");
    expect((await call("todo_update", { id: item.id, energy: "" })).energy).toBeUndefined();
    expect((await call("todo_capture", { text: "Stretch %high" })).item.energy).toBe("high");
    await expect(call("todo_update", { id: item.id, due: "someday-ish" })).rejects.toThrow(/someday-ish/);
    await expect(call("todo_update", { id: item.id })).rejects.toThrow("give at least one field to change");
  });

  it("sets and clears waiting", async () => {
    const { call } = await harness();
    const { item } = await call("todo_capture", { text: "Budget sign-off" });
    const waiting = await call("todo_update", { id: item.id, waiting_on: "Dana", follow_up: "fri" });
    expect(waiting.waiting).toMatchObject({ on: "Dana", since: "2026-11-02", follow_up_on: "2026-11-06" });
    const listed = await call("todo_list", { view: "waiting" });
    expect(listed.items).toHaveLength(1);
    const done = await call("todo_update", { id: item.id, waiting_on: null });
    expect(done.waiting).toBeUndefined();
  });

  it("completes a repeating item and returns the next instance; undo takes both back", async () => {
    const { call } = await harness();
    const { item } = await call("todo_capture", { text: "Check the build dashboard @mon" });
    expect(item).toMatchObject({ repeat: "@mon", due: "2026-11-02" });
    const { item: done, next } = await call("todo_set_status", { id: item.id, status: "done" });
    expect(done.status).toBe("done");
    // An inbox instance's next one goes to next (design 3.8).
    expect(next).toMatchObject({ title: "Check the build dashboard", repeat: "@mon", due: "2026-11-09", status: "next" });
    const undone = await call("todo_undo");
    expect(undone.undid).toBeTruthy();
    const open = await call("todo_list", { text: "dashboard" });
    expect(open.items.map((i: { id: string }) => i.id)).toEqual([item.id]);
  });

  it("sets a repeat rule from the due date, and clears it", async () => {
    const { call } = await harness();
    const { item } = await call("todo_capture", { text: "Pay rent ^2026-11-15" });
    expect((await call("todo_set_repeat", { id: item.id, rule: "monthly" })).repeat).toBe("@15th");
    expect((await call("todo_set_repeat", { id: item.id, rule: "@2w" })).repeat).toBe("@2w");
    expect((await call("todo_set_repeat", { id: item.id, rule: "" })).repeat).toBeUndefined();
    await expect(call("todo_set_repeat", { id: item.id, rule: "fortnightly" })).rejects.toThrow(/fortnightly/);
  });

  it("archives, deletes and brings items back", async () => {
    const { call } = await harness();
    const { item } = await call("todo_capture", { text: "Old idea" });
    expect((await call("todo_archive", { id: item.id })).archived).toBe(true);
    expect((await call("todo_list")).items).toHaveLength(0);
    expect((await call("todo_unarchive", { id: item.id })).archived).toBeUndefined();
    expect((await call("todo_delete", { id: item.id })).deleted).toBe(true);
    expect((await call("todo_undelete", { id: item.id })).deleted).toBeUndefined();
  });

  it("counts", async () => {
    const { call } = await harness();
    await call("todo_capture", { text: "Overdue thing ^2026-10-30" });
    await call("todo_capture", { text: "Inbox thing" });
    const status = await call("todo_status");
    expect(status).toMatchObject({ today: "2026-11-02", inbox: 2, due: 1, overdue: 1 });
    expect(status.views.Inbox).toBe(2);
  });

  it("shows the Jira link and sync state when an item has one", async () => {
    const { call } = await harness({
      items: [
        {
          id: "01JBQ0000000000000000JIRA1",
          title: "Fix login timeout",
          status: Status.NEXT,
          priority: Priority.HIGH,
          remoteLink: {
            trackerKind: "jira",
            instanceId: "work-jira",
            remoteId: "10042",
            key: "PLAT-530",
            url: "https://jira.example.com/browse/PLAT-530",
            linkState: LinkState.OK,
          },
          remoteStatusName: "In Review",
          hasConflict: true,
          pendingFields: [ItemField.TITLE],
        },
      ],
    });
    const listed = await call("todo_list", { text: "PLAT-530" });
    expect(listed.items[0]).toMatchObject({
      short_id: "000JIRA1",
      jira: { key: "PLAT-530", url: "https://jira.example.com/browse/PLAT-530", instance: "work-jira", tracker: "jira", remote_status: "In Review" },
      has_conflict: true,
      pending_fields: ["title"],
    });
  });

  it("registers only the read tools when read-only", async () => {
    const { client } = await harness({ readOnly: true });
    const { tools } = await client.listTools();
    expect(tools.map((t) => t.name).sort()).toEqual(["todo_get", "todo_list", "todo_status", "todo_views"]);
    expect(tools.every((t) => t.annotations?.readOnlyHint)).toBe(true);
  });
});
