import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, item } from "../fake.test-util.js";
import { setup, stopControllers } from "./setup.test-util.js";

afterEach(async () => {
  await stopControllers();
});

describe("M and :meet", () => {
  it("prefills the subject, opens the Teams link with what it says, and writes nothing", async () => {
    const fd = fakeDaemon([item("Plan the offsite", { waiting: { on: { ref: { case: "freeText", value: "dana@example.com" } } } }), item("budget")]);
    const opened: string[] = [];
    const t = await setup(fd, { controller: { openUrl: async (u) => void opened.push(u) } });
    await t.c.press("M");
    await vi.waitFor(() => expect(t.mode()).toMatchObject({ kind: "prompt", prompt: "meeting" }));
    expect(t.mode()).toMatchObject({ line: { text: "Plan the offsite" } });
    // No date preview: the hint stays.
    await t.c.press("x<backspace>");
    expect(t.mode()).not.toHaveProperty("preview");
    await t.c.press(" kickoff<enter>");
    await vi.waitFor(() => expect(opened).toHaveLength(1));
    expect(opened[0]).toBe("https://teams.microsoft.com/l/meeting/new?subject=Plan%20the%20offsite%20kickoff&attendees=dana%40example.com");
    expect(t.toast()).toBe("Opened the Teams meeting form");
    expect(t.mode()).toEqual({ kind: "normal", pending: [] });
    expect(t.userOps()).toEqual([]);
  });

  it("takes the marked Items, in list order; esc opens nothing", async () => {
    const opened: string[] = [];
    const t = await setup(fakeDaemon([item("a"), item("b"), item("c")]), { controller: { openUrl: async (u) => void opened.push(u) } });
    await t.c.press("mjm:meet<enter>");
    await vi.waitFor(() => expect(t.mode()).toMatchObject({ kind: "prompt", prompt: "meeting", line: { text: "a + 1 more" } }));
    await t.c.press("<esc>");
    expect(opened).toEqual([]);
  });

  it("says so when meetings are off, and keeps the prompt open with no subject", async () => {
    const fd = fakeDaemon([item("a")]);
    const opened: string[] = [];
    const t = await setup(fd, { controller: { openUrl: async (u) => void opened.push(u) } });
    await t.c.press("M");
    await vi.waitFor(() => expect(t.mode()).toMatchObject({ kind: "prompt", prompt: "meeting" }));
    await t.c.press("<ctrl+u><enter>");
    expect(t.mode()).toMatchObject({ kind: "prompt", error: "A meeting needs a subject" });
    await t.c.press("<esc>");
    fd.daemon.setConfig({ meetings: { provider: "none" } });
    await vi.waitFor(() => expect(t.store.getConfig().config?.meetings?.provider).toBe("none"));
    // M is unbound; :meet says why.
    await t.c.press("M");
    expect(t.toast()).toBe("M: not bound · ? for help");
    await t.c.press(":meet<enter>");
    await vi.waitFor(() => expect(t.toast()).toBe('Meetings are off: [meetings] provider = "none"'));
    expect(opened).toEqual([]);
  });

  it("says when Teams cannot open, with the link", async () => {
    const t = await setup(fakeDaemon([item("a")]), { controller: { openUrl: async () => Promise.reject(new Error("xdg-open: spawn xdg-open ENOENT")) } });
    await t.c.press("M");
    await vi.waitFor(() => expect(t.mode()).toMatchObject({ kind: "prompt", prompt: "meeting" }));
    await t.c.press("<enter>");
    await vi.waitFor(() => expect(t.toast()).toBe("Could not open Teams (xdg-open: spawn xdg-open ENOENT): https://teams.microsoft.com/l/meeting/new?subject=a"));
  });
});
