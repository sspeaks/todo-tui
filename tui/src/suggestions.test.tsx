// The Suggestions pane and the push state in the TUI (design 2.6, 2.2,
// 5.9, ADR-0007), over FakeDaemon.
import {
  ItemField,
  PatchState,
  Priority,
  ResolutionKind,
  Status,
  StatusCategory,
  SuggestionKind,
  SuggestionSource,
  SuggestionState,
  SyncState,
} from "@todo-tui/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, item } from "./fake.test-util.js";
import { timing } from "./controller/refine.js";
import { cleanupTui, renderTui } from "./render.test-util.js";

afterEach(async () => {
  await cleanupTui();
});

const link = (key: string) => ({
  remoteLink: { trackerKind: "jira", instanceId: "work-jira", remoteId: key.replace(/\D/g, ""), key, url: `https://jira.example.com/browse/${key}` },
});
const str = (value: string) => ({ kind: { case: "stringValue" as const, value } });
const pushing = { instances: [{ id: "work-jira", kind: "jira-dc", push: true }] };

/** PLAT-1 with a title conflict waiting: Jira's "theirs" won over "mine". */
function conflicted(extra = {}) {
  const fd = fakeDaemon([item("theirs", { ...link("PLAT-1"), isMine: true, ...extra })]);
  fd.daemon.setConfig(pushing);
  const [one] = [...fd.daemon.items.values()];
  const s = fd.daemon.putSuggestion({
    itemId: one!.id,
    kind: SuggestionKind.SYNC_CONFLICT,
    source: SuggestionSource.SYNC,
    rationale: "Jira changed the title on PLAT-1 while yours waited to push",
    patches: [{ field: ItemField.TITLE, expectedOld: str("theirs"), new: str("mine") }],
  });
  return { fd, id: one!.id, s };
}

describe("the Suggestions pane", () => {
  it("S lists what waits, unticked; enter ticks, ctrl-s applies in one undoable op", async () => {
    const { fd, id } = conflicted();
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    // The row's marker, and the count in the status bar.
    await t.waitFrame((f) => expect(f).toMatch(/✦ 1/));
    await t.waitFrame((f) => expect(f).toMatch(/theirs\s+PLAT-1\s+!/));
    await t.press("S");
    await t.waitFrame((f) => expect(f).toContain("✦ Suggestions (1 item, 1 change)"));
    expect(t.frame()).toMatch(/▾ PLAT-1 · sync_conflict · Jira changed the title on PLAT-1/);
    expect(t.frame()).toMatch(/\[ \] title\s+theirs \(Jira\) → mine \(yours\)/);
    expect(t.frame()).toMatch(/SUGGESTIONS · .*enter tick · ctrl-s apply · r reject · esc close/);
    // Nothing ticked: nothing applies.
    await t.press("<ctrl+s>");
    expect(t.ui.getState().toast?.text).toBe("Nothing ticked: enter ticks a change");
    await t.press("j<enter>");
    await t.waitFrame((f) => expect(f).toMatch(/\[x\] title/));
    await t.press("<ctrl+s>");
    await vi.waitFor(() => expect(fd.daemon.items.get(id)?.title).toBe("mine"));
    await t.waitFrame((f) => expect(f).toContain("Nothing to review"));
    expect(t.ui.getState().toast?.text).toBe('apply suggestion "mine" · u undo');
    await t.waitFrame((f) => expect(f).not.toMatch(/✦ 1/));
    // One undo entry brings Jira's title back, and the Suggestion waits again.
    await t.press("<esc>u");
    await vi.waitFor(() => expect(fd.daemon.items.get(id)?.title).toBe("theirs"));
    await t.waitFrame((f) => expect(f).toMatch(/✦ 1/));
    expect(fd.daemon.items.get(id)?.hasConflict).toBe(true);
  });

  it("enter on a heading ticks its changes; a stale one is skipped and says so", async () => {
    const { fd, id } = conflicted();
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.press("S");
    await t.waitFrame((f) => expect(f).toContain("✦ Suggestions"));
    await t.press("<enter>");
    await t.waitFrame((f) => expect(f).toMatch(/\[x\] title/));
    // Jira changes it again before the apply.
    fd.daemon.update(id, { title: "theirs 2" });
    await t.press("<ctrl+s>");
    await vi.waitFor(() => expect(t.ui.getState().toast?.text).toBe("Nothing applied · 1 skipped: changed since"));
    expect(fd.daemon.items.get(id)?.title).toBe("theirs 2");
  });

  it("r rejects the Suggestion: Jira's value stays and the marker goes", async () => {
    const { fd, id, s } = conflicted();
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.press("S");
    await t.waitFrame((f) => expect(f).toContain("✦ Suggestions"));
    await t.press("r");
    await t.waitFrame((f) => expect(f).toContain("Nothing to review"));
    expect(t.ui.getState().toast?.text).toBe("Rejected: “theirs” keeps Jira's value");
    expect(fd.daemon.suggestions.get(s.id)?.state).not.toBe(undefined);
    expect(fd.daemon.items.get(id)?.hasConflict).toBe(false);
    await t.press("<esc>");
    await t.waitFrame((f) => expect(f).toMatch(/^NORMAL/m));
  });

  it("shows a refusal's error, and a status pair as one change", async () => {
    const fd = fakeDaemon([item("Close me", { ...link("PLAT-2"), isMine: true })]);
    fd.daemon.setConfig(pushing);
    const [one] = [...fd.daemon.items.values()];
    fd.daemon.putSuggestion({
      itemId: one!.id,
      kind: SuggestionKind.PUSH_FAILED,
      source: SuggestionSource.SYNC,
      rationale: "Jira refused the status on PLAT-2",
      error: "no transition to Done from Blocked",
      patches: [
        { field: ItemField.STATUS_CATEGORY, expectedOld: { kind: { case: "statusCategory", value: StatusCategory.TODO } }, new: { kind: { case: "statusCategory", value: StatusCategory.DONE } } },
        { field: ItemField.RESOLUTION_KIND, expectedOld: { kind: { case: "cleared", value: {} } }, new: { kind: { case: "resolutionKind", value: ResolutionKind.CANCELLED } } },
      ],
    });
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.press("S");
    await t.waitFrame((f) => expect(f).toContain("✦ Suggestions (1 item, 1 change)"));
    expect(t.frame()).toMatch(/push_failed · Jira refused the status on PLAT-2/);
    expect(t.frame()).toContain("Jira: no transition to Done from Blocked");
    expect(t.frame()).toMatch(/\[ \] status\s+to do \(Jira\) → cancelled \(yours\)/);
    await t.press("j<enter><ctrl+s>");
    await vi.waitFor(() => expect(fd.daemon.items.get(one!.id)?.status).toBe(Status.CANCELLED));
  });

  it("asks before applying to someone else's issue, and y applies", async () => {
    const { fd, id } = conflicted({ isMine: false, assignee: { ref: { case: "freeText", value: "Alex" } } });
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.press("S");
    await t.waitFrame((f) => expect(f).toContain("✦ Suggestions"));
    await t.press("j<enter>");
    // Asked from inside the apply, which waits for the answer: not awaited.
    void t.press("<ctrl+s>");
    await t.waitFrame((f) => expect(f).toContain("PLAT-1 is assigned to Alex: change it in Jira? y/N"));
    // The pane stays up under the question.
    expect(t.frame()).toContain("✦ Suggestions");
    await t.press("y");
    await vi.waitFor(() => expect(fd.daemon.items.get(id)?.title).toBe("mine"));
  });

  it(":w applies from the pane's command line; elsewhere it says where", async () => {
    const { fd, id } = conflicted();
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.press(":w<enter>");
    expect(t.ui.getState().toast?.text).toBe(":w applies what the Suggestions pane has ticked: S opens it");
    await t.press("S");
    await t.waitFrame((f) => expect(f).toContain("✦ Suggestions"));
    await t.press("j<enter>:w<enter>");
    await vi.waitFor(() => expect(fd.daemon.items.get(id)?.title).toBe("mine"));
  });
});

describe("pushes in the list, the detail and the status bar", () => {
  it("↑ marks a row that waits; the detail says what waits; the status bar counts it", async () => {
    const fd = fakeDaemon([item("waits", { ...link("PLAT-3"), isMine: true, pendingFields: [ItemField.TITLE, ItemField.STATUS_CATEGORY, ItemField.RESOLUTION_KIND] })]);
    fd.daemon.setSyncStatus({ instanceId: "work-jira", state: SyncState.OFFLINE, pendingOps: 2, failedOps: 1 });
    const t = renderTui({ daemon: fd, columns: 120, rows: 24 });
    await t.live();
    await t.waitFrame((f) => expect(f).toMatch(/waits\s+PLAT-3\s+↑/));
    await t.waitFrame((f) => expect(f).toMatch(/⇅ work-jira offline ↑2 !1/));
    await t.waitFrame((f) => expect(f).toMatch(/To Jira\s+↑ the title and the status wait to\s/));
  });

  it("asks before x on someone else's issue; any key but y leaves it", async () => {
    const fd = fakeDaemon([item("theirs", { ...link("PLAT-4"), isMine: false, assignee: { ref: { case: "freeText", value: "Alex" } } })]);
    fd.daemon.setConfig(pushing);
    const [one] = [...fd.daemon.items.values()];
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    // Asked from inside the action, which waits for the answer: not awaited.
    void t.press("x");
    await t.waitFrame((f) => expect(f).toContain("PLAT-4 is assigned to Alex: change it in Jira? y/N"));
    await t.press("n");
    await vi.waitFor(() => expect(t.ui.getState().toast?.text).toBe("PLAT-4 is someone else's issue: not changed"));
    expect(fd.daemon.items.get(one!.id)?.status).not.toBe(Status.DONE);
    void t.press("x");
    await t.waitFrame((f) => expect(f).toContain("change it in Jira? y/N"));
    await t.press("y");
    await vi.waitFor(() => expect(fd.daemon.items.get(one!.id)?.status).toBe(Status.DONE));
  });

  it("a bulk edit asks once: the yes holds when a read-only Item is dropped and the rest retried", async () => {
    const fd = fakeDaemon([
      item("theirs", { ...link("PLAT-5"), isMine: false, assignee: { ref: { case: "freeText", value: "Alex" } } }),
      item("other jira", { remoteLink: { ...link("PLAT-6").remoteLink, instanceId: "other-jira" }, isMine: true }),
    ]);
    fd.daemon.setConfig(pushing);
    const [theirs] = [...fd.daemon.items.values()];
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.press("mm");
    void t.press("p1");
    await t.waitFrame((f) => expect(f).toContain("change it in Jira? y/N"));
    await t.press("y");
    await vi.waitFor(() => expect(fd.daemon.items.get(theirs!.id)?.priority).toBe(Priority.HIGHEST));
    // Asked once: the retry without PLAT-6 kept the yes.
    expect(t.frame()).not.toContain("y/N");
  });
});

describe("the pane, against what moves under it", () => {
  /** Two conflicts on two Items, A first. */
  function two() {
    const fd = fakeDaemon([item("alpha", { ...link("PLAT-7"), isMine: true }), item("beta", { ...link("PLAT-8"), isMine: true })]);
    fd.daemon.setConfig(pushing);
    const [a, b] = [...fd.daemon.items.values()];
    const put = (id: string, title: string) =>
      fd.daemon.putSuggestion({
        itemId: id,
        kind: SuggestionKind.SYNC_CONFLICT,
        source: SuggestionSource.SYNC,
        rationale: `Jira changed the title of ${title}`,
        patches: [{ field: ItemField.TITLE, expectedOld: str(title), new: str(`${title} mine`) }],
      });
    return { fd, a: a!, b: b!, sa: put(a!.id, "alpha"), sb: put(b!.id, "beta") };
  }

  it("r rejects the Suggestion the cursor is on, though another client removed one above it", async () => {
    const { fd, sa, sb } = two();
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.press("S");
    await t.waitFrame((f) => expect(f).toContain("Suggestions (2 items, 2 changes)"));
    // On beta's heading (rows: alpha, its change, beta).
    await t.press("jj");
    await t.waitFrame((f) => expect(f).toMatch(/› ▾ PLAT-8/));
    // Another client rejects alpha's meanwhile.
    await fd.clients().suggestions.rejectSuggestion({ requestId: "01J0000000000000000000000R", id: sa.id });
    await t.waitFrame((f) => expect(f).toContain("Suggestions (1 item, 1 change)"));
    await t.press("r");
    await vi.waitFor(() => expect(fd.daemon.suggestions.get(sb.id)?.state).toBe(SuggestionState.REJECTED));
  });

  it("names an Item no tab holds, and says why a closed change cannot be ticked", async () => {
    const fd = fakeDaemon([item("done one", { ...link("PLAT-9"), isMine: true, status: Status.DONE, statusCategory: StatusCategory.DONE, resolutionKind: ResolutionKind.DONE })]);
    fd.daemon.setConfig(pushing);
    const [one] = [...fd.daemon.items.values()];
    fd.daemon.putSuggestion({
      itemId: one!.id,
      kind: SuggestionKind.PUSH_FAILED,
      source: SuggestionSource.SYNC,
      rationale: "Jira refused the status on PLAT-9",
      patches: [
        {
          field: ItemField.RESOLUTION_KIND,
          expectedOld: { kind: { case: "resolutionKind", value: ResolutionKind.DONE } },
          new: { kind: { case: "resolutionKind", value: ResolutionKind.CANCELLED } },
          state: PatchState.STALE,
        },
      ],
    });
    // The Inbox does not list a done Item, so the store has not read it.
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.press("S");
    await t.waitFrame((f) => expect(f).toMatch(/▾ PLAT-9 · push_failed/));
    expect(t.frame()).toMatch(/status\s+done \(Jira\) → cancelled \(yours\)/);
    await t.press("j<enter>");
    expect(t.ui.getState().toast?.text).toBe("That change cannot be applied: its field changed since");
    await t.press("k<enter>");
    expect(t.ui.getState().toast?.text).toBe("Nothing left to apply in this Suggestion");
  });
});

describe("the NOT_MINE question's input", () => {
  function theirs() {
    const fd = fakeDaemon([item("theirs", { ...link("PLAT-10"), isMine: false, assignee: { ref: { case: "freeText", value: "Alex" } } })]);
    fd.daemon.setConfig(pushing);
    return { fd, id: [...fd.daemon.items.values()][0]!.id };
  }

  it("a key event with no key, or a paste, does not answer it", async () => {
    const { fd, id } = theirs();
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    void t.press("x");
    await t.waitFrame((f) => expect(f).toContain("PLAT-10 is assigned to Alex: change it in Jira? y/N"));
    // A bare ctrl (no key) and a paste: still asking.
    t.controller.input("", { ctrl: true });
    t.controller.paste("hello");
    expect(t.frame()).toContain("change it in Jira? y/N");
    await t.press("y");
    await vi.waitFor(() => expect(fd.daemon.items.get(id)?.status).toBe(Status.DONE));
  });

  it("quitting while an action is in flight never asks", async () => {
    const { fd, id } = theirs();
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    t.ui.setState((s) => ({ ...s, quitting: true }));
    expect(await t.controller.askNotMine(new Error("not mine"), undefined)).toBe(false);
    expect(t.frame()).not.toContain("y/N");
    expect(fd.daemon.items.get(id)?.status).not.toBe(Status.DONE);
  });
});

describe("E on a Jira item (ADR-0009)", () => {
  it("pushes the description, asks once for someone else's issue, and the yes holds across a rebase", async () => {
    const fd = fakeDaemon([item("theirs", { ...link("PLAT-11"), isMine: false, description: "## Steps", assignee: { ref: { case: "freeText", value: "Alex" } } })]);
    fd.daemon.setConfig(pushing);
    const id = [...fd.daemon.items.values()][0]!.id;
    const editor = async () => ({ ok: true as const, text: "## Mine\n" });
    const t = renderTui({ daemon: fd, columns: 120, rows: 20, controller: { editor } });
    await t.live();
    // Count the questions; while the first is asked, another field changes,
    // so the yes's resend is a revision mismatch and the save rebases.
    let asked = 0;
    const ask = t.controller.askNotMine.bind(t.controller);
    t.controller.askNotMine = (err, targets) => {
      asked += 1;
      if (asked === 1) fd.daemon.items.get(id)!.revision += 1n;
      return ask(err, targets);
    };
    void t.press("E");
    await t.waitFrame((f) => expect(f).toContain("PLAT-11 is assigned to Alex: change it in Jira? y/N"));
    await t.press("y");
    await vi.waitFor(() => expect(fd.daemon.items.get(id)?.description).toBe("## Mine"));
    expect(t.frame()).not.toContain("y/N");
    expect(asked).toBe(1);
  });

  it("dims the Jira formatting kept in a description, line by line", async () => {
    const { rawLines } = await import("./components/DetailPane.js");
    expect(rawLines("## Steps\n`wiki:{panel}`\n```wiki\n||a|b|\n```\nafter `wiki:+u+` text")).toEqual([
      { line: "## Steps", raw: false },
      { line: "`wiki:{panel}`", raw: true },
      { line: "```wiki", raw: true },
      { line: "||a|b|", raw: true },
      { line: "```", raw: true },
      { line: "after `wiki:+u+` text", raw: false },
    ]);
    // A code block's body is code, even a ```wiki or `wiki:` line in it.
    expect(rawLines("````\n```wiki\n`wiki:x`\n````\nhello").map((l) => l.raw)).toEqual([false, false, false, false, false]);
  });

  it("names the description among what waits to reach Jira", async () => {
    const fd = fakeDaemon([item("waits", { ...link("PLAT-12"), isMine: true, pendingFields: [ItemField.DESCRIPTION] })]);
    fd.daemon.setConfig(pushing);
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.waitFrame((f) => expect(f).toMatch(/To Jira\s+↑ the description waits to reach/));
  });
});

describe("space r: Claude refines (ADR-0010)", () => {
  const assistant = { assistant: { provider: "claude_code", model: "" } };
  beforeEach(() => {
    timing.pollMs = 20;
  });

  it("asks, says so, and what Claude proposes waits in S ticked; ctrl-s applies it", async () => {
    const fd = fakeDaemon([item("ping dana fri re login test")]);
    fd.daemon.setConfig(assistant);
    fd.daemon.setRefine(() => ({
      rationale: "A follow-up with Dana.",
      patches: [{ field: ItemField.TITLE, expectedOld: str("ping dana fri re login test"), new: str("Follow up with Dana on the login test") }],
    }));
    const id = [...fd.daemon.items.values()][0]!.id;
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.press("<space>r");
    await t.waitFrame((f) => expect(f).toContain("Claude proposed changes for it · S to review"));
    await t.press("S");
    await t.waitFrame((f) => expect(f).toMatch(/\[x\] title/));
    await t.press("<ctrl+s>");
    await vi.waitFor(() => expect(fd.daemon.items.get(id)?.title).toBe("Follow up with Dana on the login test"));
  });

  it("shows Claude's rationale in full, wrapped, and its sides as now and Claude's, not Jira's", async () => {
    const fd = fakeDaemon([item("ping dana")]);
    fd.daemon.setConfig(assistant);
    const why = "The day named in the text is Friday, so that is the due date, and the title says who to ask and about what so it reads clearly later.";
    fd.daemon.putSuggestion({
      itemId: [...fd.daemon.items.values()][0]!.id,
      kind: SuggestionKind.REFINE,
      source: SuggestionSource.CLAUDE,
      rationale: why,
      patches: [{ field: ItemField.TITLE, expectedOld: str("ping dana"), new: str("Ask Dana") }],
    });
    const t = renderTui({ daemon: fd, columns: 60, rows: 20 });
    await t.live();
    await t.press("S");
    await t.waitFrame((f) => expect(f).toMatch(/▾ ping dana · Claude\s*$/m));
    // Every word of the rationale is on screen, over several lines.
    const frame = t.frame();
    for (const word of ["Friday,", "clearly", "later."]) expect(frame).toContain(word);
    expect(frame).toMatch(/\[x\] title +ping dana \(now\) → Ask Dana \(Claude\)/);
    expect(frame).not.toContain("(Jira)");
  });

  it("says when Claude fails or is off, and space r is no key while it is off", async () => {
    const fd = fakeDaemon([item("x")]);
    fd.daemon.setConfig(assistant);
    fd.daemon.setRefine(() => ({ error: "Claude Code is not logged in: run `claude` once to log in" }));
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.press("<space>r");
    await t.waitFrame((f) => expect(f).toContain("Claude could not refine it: Claude Code is not logged in"));
    fd.daemon.setConfig({});
    await vi.waitFor(() => expect(t.store.getConfig().config?.assistant?.provider ?? "").toBe(""));
    await t.press(":refine<enter>");
    await t.waitFrame((f) => expect(f).toContain("Claude is off"));
  });
});

describe("P and L: promote and attach (design 5.7, 3.3)", () => {
  const jira = { instances: [{ id: "work-jira", kind: "jira-dc", push: true, promoteProject: "PLAT", promoteIssueType: "Task" }] };

  it("P creates the issue in promote_default's project and says so", async () => {
    const fd = fakeDaemon([item("Ship the report")]);
    fd.daemon.setConfig(jira);
    const id = [...fd.daemon.items.values()][0]!.id;
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.press("P");
    await t.waitFrame((f) => expect(f).toContain("Create in Jira project: PLAT"));
    await t.press("<enter>");
    await t.waitFrame((f) => expect(f).toMatch(/Created PLAT-\d+/));
    expect(fd.daemon.items.get(id)?.remoteLink?.pinned).toBe(true);
  });

  it("L links to a key, Jira's title wins, and yours waits in S", async () => {
    const fd = fakeDaemon([item("look at login")]);
    fd.daemon.setConfig(jira);
    fd.daemon.setIssue("PLAT-7", "Fix the login test");
    const id = [...fd.daemon.items.values()][0]!.id;
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.press("L");
    await t.press("nope<enter>");
    await t.waitFrame((f) => expect(f).toContain("An issue key, e.g. PLAT-12"));
    await t.press("<ctrl+u>plat-7<enter>");
    await t.waitFrame((f) => expect(f).toContain("Linked to PLAT-7 · what Jira changed of yours is in S"));
    expect(fd.daemon.items.get(id)?.title).toBe("Fix the login test");
    await t.press("S");
    await t.waitFrame((f) => expect(f).toMatch(/\[ \] title +Fix the login test \(Jira\) → look at login \(yours\)/));
  });

  it("says why when it cannot", async () => {
    const fd = fakeDaemon([item("x")]);
    fd.daemon.setConfig({ instances: [{ id: "work-jira", kind: "jira-dc", push: false }] });
    const t = renderTui({ daemon: fd, columns: 120, rows: 20 });
    await t.live();
    await t.press("P");
    await t.press("PLAT<enter>");
    await t.waitFrame((f) => expect(f).toContain("Not created: Jira is read-only here (push = false)"));
  });
});
