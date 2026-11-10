import { Priority, Status, SyncState, timestampFromDate } from "@todo-tui/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, item } from "../fake.test-util.js";
import { ago, stateWord, statusLine } from "./sync.js";
import { setup, stopControllers } from "./setup.test-util.js";

afterEach(async () => {
  await stopControllers();
});

const link = (key: string) => ({
  remoteLink: { trackerKind: "jira", instanceId: "work-jira", remoteId: key.replace(/\D/g, ""), key, url: `https://jira.example.com/browse/${key}` },
});

const instance = { instances: [{ id: "work-jira", kind: "jira-dc", baseUrl: "https://jira.example.com" }] };

describe("o, gs and :sync", () => {
  it("o opens the linked Item's issue, and says when there is none", async () => {
    const opened: string[] = [];
    const t = await setup(fakeDaemon([item("PLAT-1 work", link("PLAT-1")), item("local")]), { controller: { openUrl: async (u) => void opened.push(u) } });
    await t.c.press("o");
    await vi.waitFor(() => expect(opened).toEqual(["https://jira.example.com/browse/PLAT-1"]));
    expect(t.toast()).toBe("Opened PLAT-1");
    await t.c.press("jo");
    await vi.waitFor(() => expect(t.toast()).toBe("Not linked to Jira"));
    expect(opened).toHaveLength(1);
    expect(t.userOps()).toEqual([]);
  });

  it("gs and :sync ask for a pull; with nothing configured they say so", async () => {
    const fd = fakeDaemon([item("a")]);
    const t = await setup(fd);
    await t.c.press("gs");
    await vi.waitFor(() => expect(t.toast()).toBe("Nothing to sync: no [[instances]] in the config"));
    fd.daemon.setConfig(instance);
    await t.c.press("gs");
    await vi.waitFor(() => expect(t.toast()).toBe("Syncing…"));
    await t.c.press(":sync work-jira<enter>");
    await vi.waitFor(() => expect(t.toast()).toBe("Syncing work-jira…"));
    expect(fd.daemon.syncNows).toEqual(["", "work-jira"]);
  });

  it(":sync status says how each instance stands, from the notices", async () => {
    const fd = fakeDaemon([item("a")]);
    const t = await setup(fd);
    await t.c.press(":sync status<enter>");
    expect(t.toast()).toBe("Nothing syncs: no [[instances]] in the config");
    fd.daemon.setSyncStatus({ instanceId: "work-jira", state: SyncState.AUTH_ERROR, lastError: "401 Unauthorized", items: 3 });
    await vi.waitFor(() => expect(t.store.getSyncStatus().size).toBe(1));
    await t.c.press(":sync status<enter>");
    expect(t.state().toast).toMatchObject({
      level: "error",
      text: "work-jira: the token was refused: store a new one, then gs (401 Unauthorized) · 3 items · not pulled yet",
    });
  });
});

describe("the read-only guard", () => {
  it("toasts the refusal with the issue's key and the field, and Overlay edits still save", async () => {
    const t = await setup(fakeDaemon([item("PLAT-7 work", link("PLAT-7"))]));
    await t.c.press("x");
    await vi.waitFor(() => expect(t.toast()).toBe("PLAT-7: the status is Jira's: change it there (o opens it)"));
    await t.c.press("eh");
    await vi.waitFor(() => expect(t.userOps()).toHaveLength(1));
  });

  it("a bulk edit leaves the linked Items out, and says how many", async () => {
    const t = await setup(fakeDaemon([item("PLAT-1 work", link("PLAT-1")), item("local a"), item("PLAT-2 work", link("PLAT-2")), item("local b")]));
    await t.c.press("mmmm");
    await t.c.press("p1");
    await vi.waitFor(() => expect(t.toast()).toBe("edit 2 items · u undo · 2 Jira items left as they are (Jira's)"));
    expect(t.byTitle("local a")?.priority).toBe(Priority.HIGHEST);
    expect(t.byTitle("PLAT-1 work")?.priority).not.toBe(Priority.HIGHEST);
  });

  it("a bulk status leaves out only the Jira items it would change in Jira", async () => {
    // next on a Jira to-do is the local refinement; on one in progress it is Jira's.
    const t = await setup(
      fakeDaemon([item("PLAT-1 todo", link("PLAT-1")), item("PLAT-2 todo doing", { ...link("PLAT-2"), status: Status.IN_PROGRESS }), item("todo local")]),
    );
    // Every one of them, in progress too.
    await t.c.press(":search todo<enter>");
    await vi.waitFor(() => expect(t.titles()).toHaveLength(3));
    await t.c.press("mmm");
    await t.c.press("sn");
    await vi.waitFor(() => expect(t.toast()).toBe("move to next (2 items) · u undo · 1 Jira item left as it is (Jira's)"));
    expect(t.byTitle("PLAT-1 todo")?.status).toBe(Status.NEXT);
    expect(t.byTitle("PLAT-2 todo doing")?.status).toBe(Status.IN_PROGRESS);
    expect(t.byTitle("todo local")?.status).toBe(Status.NEXT);
  });

  it("a bulk edit of only linked Items is refused whole", async () => {
    const t = await setup(fakeDaemon([item("PLAT-1 work", link("PLAT-1")), item("PLAT-2 work", link("PLAT-2"))]));
    await t.c.press("mm");
    await t.c.press("p1");
    await vi.waitFor(() => expect(t.toast()).toBe("2 Jira items: that is Jira's, change it there"));
    expect(t.userOps()).toEqual([]);
  });
});

describe("the sync words", () => {
  const now = new Date(Date.UTC(2026, 10, 5, 12, 0, 0));
  const at = (s: number) => timestampFromDate(new Date(now.getTime() - s * 1000));

  it("ages the last pull", () => {
    expect([30, 120, 7200, 172_800].map((s) => ago(new Date(now.getTime() - s * 1000), now))).toEqual(["now", "2m", "2h", "2d"]);
  });

  it("names each state", () => {
    const of = (state: SyncState, extra = {}) => stateWord({ instanceId: "j", state, lastError: "", items: 0, pulled: 0, ...extra } as never, now);
    expect(of(SyncState.OK, { lastSuccessfulPullAt: at(120) })).toBe("2m");
    expect(of(SyncState.SYNCING, { pulled: 40 })).toBe("syncing… 40");
    expect([SyncState.OFFLINE, SyncState.AUTH_ERROR, SyncState.DEGRADED, SyncState.STARTING].map((s) => of(s))).toEqual(["offline", "auth", "error", "starting"]);
    expect(statusLine({ instanceId: "j", state: SyncState.OK, lastError: "", items: 1, pulled: 0, lastSuccessfulPullAt: at(30) } as never, now)).toBe("j: ok · 1 item · last pull now");
  });
});
