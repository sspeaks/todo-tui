import { Status, Weekday, setRecurrence, setStatus, updateItem } from "@todo-tui/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, item, rows } from "../fake.test-util.js";
import { MIDNIGHT_POLL_MS } from "./controller.js";
import { setup, stopControllers } from "./setup.test-util.js";

afterEach(async () => {
  await stopControllers();
  vi.useRealTimers();
});

/** Monday 2026-10-26, noon UTC (the tests run with TZ=UTC). */
const NOON = new Date(Date.UTC(2026, 9, 26, 12));
const clock = () => NOON;
const date = (day: number, month = 10) => ({ year: 2026, month, day });
const dated = (init: Parameters<typeof item>[0][] | ReturnType<typeof item>[]) =>
  fakeDaemon(init.map((i) => (typeof i === "string" ? item(i) : i)), { now: clock, timeZone: "UTC" });

/** The open prompt, once its preview has caught up with its text. */
async function prompt(t: Awaited<ReturnType<typeof setup>>) {
  await t.c.previewIdle();
  const mode = t.mode();
  if (mode.kind !== "prompt") throw new Error(`not a prompt: ${mode.kind}`);
  return mode;
}

describe("^ and D", () => {
  it("previews the date, sets it, clears it on empty, and keeps the prompt on a warning (hard case 45)", async () => {
    const t = await setup(dated(["X", "Y"]), { now: clock });
    await t.c.press("^");
    expect((await prompt(t)).preview).toMatchObject({ text: "enter clears the due date", warn: false });
    await t.c.press("fri");
    expect((await prompt(t)).preview).toMatchObject({ for: "fri", text: "→ Fri Oct 30", warn: false });
    await t.c.press("<enter>");
    expect(t.mode().kind).toBe("normal");
    expect(t.byTitle("X")?.dueDate).toMatchObject(date(30));
    // A plain edit that keeps the row: no toast.
    expect(t.toast()).toBeUndefined();

    // Prefilled with the ISO date; emptied, it says it clears, and does.
    await t.c.press("^");
    expect((await prompt(t)).line).toEqual({ text: "2026-10-30", cursor: 10 });
    await t.c.press("<ctrl+u>");
    expect((await prompt(t)).preview?.text).toBe("enter clears the due date");
    await t.c.press("<enter>");
    expect(t.byTitle("X")?.dueDate).toBeUndefined();

    await t.c.press("^bogus");
    expect((await prompt(t)).preview).toMatchObject({ text: "bogus: not a date", warn: true });
    await t.c.press("<enter>");
    const kept = await prompt(t);
    expect(kept).toMatchObject({ prompt: "due", line: { text: "bogus" }, preview: { text: "bogus: not a date", warn: true } });
    expect(t.userOps()).toHaveLength(2);
    // esc cancels.
    await t.c.press("<esc>");
    expect(t.mode().kind).toBe("normal");
  });

  it("D defers: the Item leaves the tab, toasted", async () => {
    const t = await setup(dated(["X", "Y"]), { now: clock });
    await t.c.press("Dnov2<enter>");
    expect(t.byTitle("X")?.deferUntil).toMatchObject(date(2, 11));
    expect(t.titles()).toEqual(["Y"]);
    expect(t.toast()).toBe('edit "X" · u undo');
  });

  it("applies to the Item it opened on, though another row came under the cursor meanwhile (hard case 46)", async () => {
    const t = await setup(dated(["X", "Y"]), { now: clock });
    await t.c.press("^fri");
    const x = t.byTitle("X")!.id;
    await setStatus(t.clients().items, x, Status.DONE);
    await vi.waitFor(() => expect(t.selected()).toBe("Y"));
    await t.c.press("<enter>");
    expect(t.byTitle("X")?.dueDate).toMatchObject(date(30));
    expect(t.byTitle("Y")?.dueDate).toBeUndefined();
  });
});

describe("@", () => {
  const weekly = (...days: Weekday[]) => ({ kind: { case: "weekly" as const, value: { days } } });

  it("previews the rule and its first date, sets it, keeps the prompt on a warning, and stops it on empty", async () => {
    const t = await setup(dated(["water plants", "Y"]), { now: clock });
    await t.c.press("@");
    expect(await prompt(t)).toMatchObject({ prompt: "repeat", line: { text: "" }, preview: { text: "enter stops repeating", warn: false } });
    await t.c.press("mon,thu");
    expect((await prompt(t)).preview).toMatchObject({ for: "mon,thu", text: "→ every Mon, Thu · first Mon Oct 26", warn: false });
    await t.c.press("<enter>");
    expect(t.mode().kind).toBe("normal");
    // No due date: the first date becomes it.
    expect(t.byTitle("water plants")).toMatchObject({ dueDate: date(26), recurrence: { rule: weekly(Weekday.MONDAY, Weekday.THURSDAY), occurrenceDate: date(26) } });
    expect(t.userOps().map((o) => o.summary)).toEqual(['set repeat "water plants"']);
    expect(t.toast()).toBeUndefined();

    // Prefilled with the rule's token; the same rule again sends nothing.
    await t.c.press("@");
    expect((await prompt(t)).line).toEqual({ text: "mon,thu", cursor: 7 });
    await t.c.press("<enter>");
    expect(t.userOps()).toHaveLength(1);

    await t.c.press("@<ctrl+u>@home");
    expect((await prompt(t)).preview).toMatchObject({ text: "@home: not a repeat rule", warn: true });
    await t.c.press("<enter>");
    expect(await prompt(t)).toMatchObject({ line: { text: "@home" }, preview: { text: "@home: not a repeat rule", warn: true } });
    await t.c.press("<ctrl+u>3d");
    expect((await prompt(t)).preview?.text).toBe("→ 3 days after done · first Mon Oct 26");
    await t.c.press("<enter>");
    expect(t.byTitle("water plants")?.recurrence?.rule).toEqual(expect.objectContaining({ kind: { case: "afterCompletion", value: expect.objectContaining({ days: 3 }) } }));
    await t.c.press("@");
    expect((await prompt(t)).line.text).toBe("3d");
    await t.c.press("<ctrl+u><enter>");
    expect(t.byTitle("water plants")?.recurrence).toBeUndefined();
    expect(t.userOps().at(-1)?.summary).toBe('stop repeating "water plants"');
    // Empty on an Item that does not repeat sends nothing.
    await t.c.press("@<enter>");
    expect(t.userOps()).toHaveLength(3);
  });

  it("resolves weekly and monthly against the Item's due date, and prefills its day", async () => {
    const t = await setup(dated([item("X", { dueDate: date(29) })]), { now: clock });
    await t.c.press("@weekly");
    expect((await prompt(t)).preview?.text).toBe("→ every Thu · first Thu Oct 29");
    await t.c.press("<ctrl+u>monthly");
    expect((await prompt(t)).preview?.text).toBe("→ monthly on the 29th · first Thu Oct 29");
    await t.c.press("<enter>");
    expect(t.byTitle("X")?.recurrence?.rule?.kind).toMatchObject({ case: "monthly", value: { day: 29 } });
    await t.c.press("@");
    expect((await prompt(t)).line.text).toBe("29th");
    await t.c.press("<ctrl+u>weekdays<enter>@");
    expect((await prompt(t)).line.text).toBe("weekdays");
  });

  it("previews weekly again when the due date it resolves against moves elsewhere, so enter saves what it showed", async () => {
    const t = await setup(dated(["X"]), { now: clock });
    const id = t.byTitle("X")!.id;
    await t.c.press("@weekly");
    expect((await prompt(t)).preview?.text).toBe("→ every Mon · first Mon Oct 26");
    await updateItem(t.clients().items, { id, dueDate: date(29) }, ["due_date"]);
    await vi.waitFor(() => expect(t.store.getItem(id)?.dueDate).toMatchObject(date(29)));
    expect((await prompt(t)).preview?.text).toBe("→ every Thu · first Thu Oct 29");
    await t.c.press("<enter>");
    expect(t.byTitle("X")?.recurrence?.rule?.kind).toMatchObject({ case: "weekly", value: { days: [Weekday.THURSDAY] } });
  });

  it("saves weekly on an Item with no due date on the day shown, though midnight passed since; the poll's new today previews again", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    let now = new Date(Date.UTC(2026, 9, 26, 23, 59));
    const t = await setup(fakeDaemon([item("X"), item("Y")], { now: () => now, timeZone: "UTC" }), { now: () => now });
    await vi.waitFor(() => expect(t.store.getCounts().today).toMatchObject(date(26)));
    const [x, y] = [t.byTitle("X")!.id, t.byTitle("Y")!.id];
    await t.c.press("@weekly");
    expect((await prompt(t)).preview?.text).toBe("→ every Mon · first Mon Oct 26");
    // The daemon's midnight, before the poll has seen it: enter saves the Monday shown.
    now = new Date(Date.UTC(2026, 9, 27, 0, 0, 30));
    await t.c.press("<enter>");
    expect(t.mode().kind).toBe("normal");
    expect(t.daemon.items.get(x)?.recurrence?.rule?.kind).toMatchObject({ case: "weekly", value: { days: [Weekday.MONDAY] } });

    await t.c.press("j@weekly");
    expect((await prompt(t)).preview?.text).toBe("→ every Mon · first Mon Oct 26");
    // The poll's refresh: the preview follows the new today, and enter saves it.
    vi.advanceTimersByTime(MIDNIGHT_POLL_MS);
    await vi.waitFor(() => expect(t.store.getCounts().today).toMatchObject(date(27)));
    await vi.waitFor(async () => expect((await prompt(t)).preview?.text).toBe("→ every Tue · first Tue Oct 27"));
    await t.c.press("<enter>");
    expect(t.daemon.items.get(y)?.recurrence?.rule?.kind).toMatchObject({ case: "weekly", value: { days: [Weekday.TUESDAY] } });
  });

  it("keeps the prompt on enter when the Item read back is due on another day than the preview counted from", async () => {
    const t = await setup(dated(["X", "Y"]), { now: clock });
    const id = t.byTitle("X")!.id;
    await t.c.press("@weekly");
    expect((await prompt(t)).preview?.text).toBe("→ every Mon · first Mon Oct 26");
    // Due and deferred elsewhere while the stream was down, then a resync: the store no longer holds it.
    t.daemon.down = true;
    t.dropStreams();
    await vi.waitFor(() => expect(t.store.getConnection().state).toBe("reconnecting"));
    t.daemon.update(id, { dueDate: date(29), deferUntil: date(2, 11) });
    t.daemon.truncateChanges();
    t.daemon.down = false;
    await vi.waitFor(() => expect(t.store.getItem(id)).toBeUndefined());
    await t.c.press("<enter>");
    expect(await prompt(t)).toMatchObject({ prompt: "repeat", line: { text: "weekly" }, preview: { text: "→ every Thu · first Thu Oct 29" } });
    expect(t.daemon.items.get(id)?.recurrence).toBeUndefined();
    await t.c.press("<enter>");
    expect(t.mode().kind).toBe("normal");
    expect(t.daemon.items.get(id)?.recurrence?.rule?.kind).toMatchObject({ case: "weekly", value: { days: [Weekday.THURSDAY] } });
  });

  it("says a done Item must be reopened first, keeping the rule as it was", async () => {
    const t = await setup(dated(["X"]), { now: clock });
    const id = t.byTitle("X")!.id;
    await t.c.press("@daily");
    await setStatus(t.clients().items, id, Status.DONE);
    await t.c.press("<enter>");
    expect(t.state().toast).toMatchObject({ level: "error", text: "“X” is done or cancelled: reopen it first" });
    expect(t.byTitle("X")?.recurrence).toBeUndefined();
  });

  it("x on a recurring Item toasts the next date, and the next instance is in the store", async () => {
    const t = await setup(dated(["water plants"]), { now: clock });
    const id = t.byTitle("water plants")!.id;
    await setRecurrence(t.clients().items, id, weekly(Weekday.MONDAY, Weekday.THURSDAY));
    await vi.waitFor(() => expect(t.store.getItem(id)?.recurrence).toBeDefined());
    const applied = vi.spyOn(t.store, "applyItems");
    await t.c.press("x");
    expect(t.toast()).toBe('mark done "water plants" · next Thu Oct 29 · u undo');
    const next = [...t.daemon.items.values()].find((i) => i.title === "water plants" && i.id !== id)!;
    expect(next).toMatchObject({ dueDate: date(29), deferUntil: date(29), status: Status.NEXT });
    expect(t.store.getItem(next.id)?.recurrence?.occurrenceDate).toMatchObject(date(29));
    // With the op's response (SetStatus's generated), not only once the stream delivers it.
    expect(applied.mock.calls.flatMap(([items]) => items.map((i) => i.id))).toContain(next.id);
  });
});

describe("i", () => {
  it("edits the title, prefilled; unchanged closes without a call, empty is refused", async () => {
    const t = await setup(fakeDaemon(rows(2)));
    await t.c.press("i");
    expect(t.mode()).toMatchObject({ kind: "prompt", prompt: "title", line: { text: "row 0", cursor: 5 } });
    await t.c.press("<enter>");
    expect(t.mode().kind).toBe("normal");
    expect(t.userOps()).toEqual([]);
    await t.c.press("i<ctrl+u><enter>");
    expect(t.mode()).toMatchObject({ kind: "prompt", error: "The title cannot be empty" });
    await t.c.press("call the dentist<enter>");
    expect(t.mode().kind).toBe("normal");
    expect(t.byTitle("call the dentist")).toBeDefined();
    expect(t.toast()).toBeUndefined();
    // Sent with the revision it was made from.
    expect(t.userOps().map((o) => o.summary)).toEqual(['edit "call the dentist"']);
  });

  it("keeps the text on a rename elsewhere, says to what, and the next enter overwrites (hard case 33)", async () => {
    const t = await setup(fakeDaemon(rows(2)));
    await t.c.press("i<ctrl+u>mine");
    const id = t.c.selectedId()!;
    await updateItem(t.clients().items, { id, title: "theirs" }, ["title"]);
    await t.c.press("<enter>");
    expect(t.mode()).toMatchObject({ kind: "prompt", prompt: "title", line: { text: "mine" }, conflict: "theirs" });
    expect(t.store.getItem(id)?.title).toBe("theirs");
    await t.c.press("<enter>");
    expect(t.mode().kind).toBe("normal");
    expect(t.store.getItem(id)?.title).toBe("mine");
    expect(t.daemon.items.get(id)?.title).toBe("mine");
  });

  it("esc after a conflict discards the text", async () => {
    const t = await setup(fakeDaemon(rows(1)));
    await t.c.press("i<ctrl+u>mine");
    const id = t.c.selectedId()!;
    await updateItem(t.clients().items, { id, title: "theirs" }, ["title"]);
    // A key typed behind the enter that hit the conflict is dropped, not typed into the text.
    await t.c.press("<enter>x");
    expect(t.mode()).toMatchObject({ kind: "prompt", line: { text: "mine" }, conflict: "theirs" });
    expect(t.toast()).toBe("1 key dropped");
    await t.c.press("<esc>");
    expect(t.mode().kind).toBe("normal");
    expect(t.daemon.items.get(id)?.title).toBe("theirs");
  });

  it("keeps the text when its Item left the store meanwhile: saves it if the Item is still there, else says it went", async () => {
    const t = await setup(fakeDaemon(rows(3)));
    await t.c.press("i<ctrl+u>renamed");
    const id = t.c.selectedId()!;
    // Done elsewhere while the stream was down, then a resync: the store keeps only what its views hold.
    t.daemon.down = true;
    t.dropStreams();
    await vi.waitFor(() => expect(t.store.getConnection().state).toBe("reconnecting"));
    t.daemon.update(id, { status: Status.DONE });
    t.daemon.truncateChanges();
    t.daemon.down = false;
    await vi.waitFor(() => expect(t.store.getItem(id)).toBeUndefined());
    await t.c.press("<enter>");
    expect(t.mode().kind).toBe("normal");
    expect(t.daemon.items.get(id)?.title).toBe("renamed");

    // Removed outright: nowhere to save it, so the prompt stays with the text.
    await t.c.press("i<ctrl+u>lost?");
    const gone = t.c.selectedId()!;
    t.daemon.remove(gone);
    await vi.waitFor(() => expect(t.store.getItem(gone)).toBeUndefined());
    await t.c.press("<enter>");
    expect(t.mode()).toMatchObject({ kind: "prompt", line: { text: "lost?" }, error: "The Item was deleted elsewhere" });
  });

  it("rebases silently when only another field changed (hard case 34)", async () => {
    const t = await setup(fakeDaemon(rows(2)));
    await t.c.press("i<ctrl+u>mine");
    const id = t.c.selectedId()!;
    await updateItem(t.clients().items, { id, priority: 2 }, ["priority"]);
    await t.c.press("<enter>");
    expect(t.mode().kind).toBe("normal");
    expect(t.daemon.items.get(id)).toMatchObject({ title: "mine", priority: 2 });
    expect(t.toast()).toBeUndefined();
  });
});

describe("t", () => {
  it("edits the labels as a whole set, prefilled", async () => {
    const t = await setup(fakeDaemon([item("X", { labels: ["home", "errand"] })]));
    await t.c.press("t");
    expect(t.mode()).toMatchObject({ kind: "prompt", prompt: "labels", line: { text: "#home #errand " } });
    await t.c.press("#car<enter>");
    expect(t.byTitle("X")?.labels).toEqual(["home", "errand", "car"]);
    // The same set in another order or with #s left out is no change.
    await t.c.press("t<ctrl+u>car errand #home<enter>");
    expect(t.userOps()).toHaveLength(1);
    await t.c.press("t<ctrl+w><ctrl+w><enter>");
    expect(t.byTitle("X")?.labels).toEqual(["home"]);
  });

  it("keeps a label that starts with #, as capture makes them, when another label is added", async () => {
    const t = await setup(fakeDaemon([item("X", { labels: ["#urgent", "home"] })]));
    await t.c.press("t");
    expect(t.mode()).toMatchObject({ kind: "prompt", prompt: "labels", line: { text: "##urgent #home " } });
    await t.c.press("work<enter>");
    expect(t.byTitle("X")?.labels).toEqual(["#urgent", "home", "work"]);
  });

  it("shows a set changed elsewhere, and rebases when only another field changed", async () => {
    const t = await setup(fakeDaemon([item("X", { labels: ["a"] })]));
    const id = t.c.selectedId()!;
    await t.c.press("tb");
    await updateItem(t.clients().items, { id, labels: ["a", "z"] }, ["labels"]);
    await t.c.press("<enter>");
    expect(t.mode()).toMatchObject({ kind: "prompt", conflict: "#a #z", line: { text: "#a b" } });
    await t.c.press("<enter>");
    expect(t.daemon.items.get(id)?.labels).toEqual(["a", "b"]);
    await t.c.press("tc");
    await updateItem(t.clients().items, { id, title: "Y" }, ["title"]);
    await t.c.press("<enter>");
    expect(t.daemon.items.get(id)).toMatchObject({ title: "Y", labels: ["a", "b", "c"] });
  });
});

describe("w and W", () => {
  const waiting = (on: string, since = date(20), followUpOn?: ReturnType<typeof date>) => ({
    on: { ref: { case: "freeText" as const, value: on } },
    since,
    ...(followUpOn && { followUpOn }),
  });

  it("completes the person with tab from the people waited on, and sets waiting since today with the follow-up", async () => {
    const fd = dated([
      item("X"),
      item("a", { status: Status.NEXT, waiting: waiting("Dana Scully") }),
      item("b", { status: Status.DONE, waiting: waiting("dave") }),
      item("c", { status: Status.NEXT, waiting: waiting("dana scully") }),
    ]);
    const t = await setup(fd, { now: clock });
    await t.c.press("w");
    expect((await prompt(t)).people?.map((p) => p.name)).toEqual(["Dana Scully", "dave"]);
    // "da" is all they share: tab does nothing; "dan" is one person.
    await t.c.press("da<tab>");
    expect((await prompt(t)).line.text).toBe("da");
    await t.c.press("n<tab>");
    expect((await prompt(t)).line).toEqual({ text: "Dana Scully ", cursor: 12 });
    await t.c.press("^fri");
    expect((await prompt(t)).preview).toMatchObject({ text: "→ ⧗Dana Scully · follow up Fri Oct 30", warn: false });
    await t.c.press("<enter>");
    expect(t.byTitle("X")?.waiting).toMatchObject(waiting("Dana Scully", date(26), date(30)));
  });

  it("w then tab in one chunk completes: the people are read before the next key", async () => {
    const t = await setup(dated([item("X"), item("a", { waiting: waiting("Mulder") })]), { now: clock });
    t.c.input("wM\t", {});
    await t.c.idle();
    expect((await prompt(t)).line.text).toBe("Mulder ");
  });

  it("the same person changes only the follow-up and keeps since; another person starts over; empty clears", async () => {
    const t = await setup(dated([item("X", { waiting: waiting("dana", date(20), date(28)) })]), { now: clock });
    await t.c.press("w");
    expect((await prompt(t)).line.text).toBe("dana ^2026-10-28");
    await t.c.press("<ctrl+w>^mon<enter>");
    expect(t.byTitle("X")?.waiting).toMatchObject(waiting("dana", date(20), date(2, 11)));
    await t.c.press("w<ctrl+u>fox<enter>");
    expect(t.byTitle("X")?.waiting).toMatchObject({ on: { ref: { value: "fox" } }, since: date(26) });
    expect(t.byTitle("X")?.waiting?.followUpOn).toBeUndefined();
    await t.c.press("w<ctrl+u>");
    expect((await prompt(t)).preview?.text).toBe("enter clears waiting");
    await t.c.press("<enter>");
    expect(t.byTitle("X")?.waiting).toBeUndefined();
  });

  it("completes and saves a known Person by id, and the preview names them with their address", async () => {
    const fd = dated([item("X")]);
    const dana = fd.daemon.putPerson({ displayName: "Dana Example", username: "dana", emails: ["dana@example.com"] });
    const t = await setup(fd, { now: clock });
    await vi.waitFor(() => expect(t.store.people.get(dana.id)).toBeDefined());
    await t.c.press("w");
    expect((await prompt(t)).people?.map((p) => [p.name, p.personId])).toEqual([["Dana Example", dana.id]]);
    await t.c.press("Da<tab>");
    expect((await prompt(t)).line.text).toBe("Dana Example ");
    expect((await prompt(t)).preview?.text).toBe("→ ⧗Dana Example <dana@example.com>");
    await t.c.press("<enter>");
    expect(t.byTitle("X")?.waiting?.on?.ref).toEqual({ case: "personId", value: dana.id });
    // Prefilled with her name; the same name keeps her and changes only the follow-up.
    await t.c.press("w");
    expect((await prompt(t)).line.text).toBe("Dana Example");
    await t.c.press(" ^fri<enter>");
    expect(t.byTitle("X")?.waiting).toMatchObject({ on: { ref: { case: "personId", value: dana.id } }, since: date(26), followUpOn: date(30) });
    // A name one person answers to links them too.
    await t.c.press("w<ctrl+u>dana<enter>");
    expect(t.byTitle("X")?.waiting?.on?.ref).toEqual({ case: "personId", value: dana.id });
  });

  it("tab with no one here asks Jira's directory, and enter adds the person found", async () => {
    const fd = dated([item("X")]);
    fd.daemon.putDirectoryUser({ accountId: "JIRAUSER3", displayName: "Jordan Lee", username: "jlee", email: "jordan.lee@example.com" });
    const t = await setup(fd, { now: clock });
    await t.c.press("w");
    await prompt(t);
    await t.c.press("jor<tab>");
    await vi.waitFor(() => expect(t.mode()).toMatchObject({ line: { text: "Jordan Lee " } }));
    expect((await prompt(t)).preview?.text).toBe("→ ⧗Jordan Lee <jordan.lee@example.com> (new, from Jira)");
    await t.c.press("<enter>");
    await vi.waitFor(() => expect(t.byTitle("X")?.waiting).toBeDefined());
    const jordan = [...fd.daemon.people.values()].find((p) => p.username === "jlee");
    expect(jordan?.emails).toEqual(["jordan.lee@example.com"]);
    expect(t.byTitle("X")?.waiting?.on?.ref).toEqual({ case: "personId", value: jordan?.id });
  });

  it("the same person typed another way keeps since and the reason", async () => {
    const fd = dated([item("X")]);
    const dana = fd.daemon.putPerson({ displayName: "Dana Example", username: "dana" });
    const x = [...fd.daemon.items.values()][0]!;
    fd.daemon.update(x.id, { waiting: { on: { ref: { case: "personId", value: dana.id } }, since: date(1), reason: "the quote" } });
    const t = await setup(fd, { now: clock });
    await vi.waitFor(() => expect(t.store.people.get(dana.id)).toBeDefined());
    await t.c.press("w<ctrl+u>dana ^fri<enter>");
    expect(t.byTitle("X")?.waiting).toMatchObject({ on: { ref: { case: "personId", value: dana.id } }, since: date(1), reason: "the quote", followUpOn: date(30) });
  });

  it("a surname the directory found completes to the one person it found", async () => {
    const fd = dated([item("X")]);
    fd.daemon.putDirectoryUser({ accountId: "JIRAUSER3", displayName: "Jordan Smith", username: "jsmith", email: "jordan.smith@example.com" });
    const t = await setup(fd, { now: clock });
    await t.c.press("w");
    await prompt(t);
    await t.c.press("smith<tab>");
    await vi.waitFor(() => expect(t.mode()).toMatchObject({ line: { text: "Jordan Smith " } }));
  });

  it("tab with no one here and Jira away keeps free text and says why", async () => {
    const t = await setup(dated([item("X")]), { now: clock });
    await t.c.press("w");
    await prompt(t);
    await t.c.press("the vendor<tab>");
    await vi.waitFor(() => expect(t.mode()).toMatchObject({ error: "No one here by that name, and Jira's directory could not be asked" }));
    await t.c.press("<enter>");
    expect(t.byTitle("X")?.waiting?.on?.ref).toEqual({ case: "freeText", value: "the vendor" });
  });

  it("refuses a follow-up without a person, and keeps the prompt on a date that is not one", async () => {
    const t = await setup(dated(["X"]), { now: clock });
    await t.c.press("w^fri<enter>");
    expect(t.mode()).toMatchObject({ kind: "prompt", error: "Who is it waiting on? Type a name before the ^date" });
    await t.c.press("<home>dana <end><ctrl+w>^nope<enter>");
    expect(await prompt(t)).toMatchObject({ line: { text: "dana ^nope" }, preview: { text: "^nope: not a date", warn: true } });
    expect(t.userOps()).toEqual([]);
  });
});

describe("prompts", () => {
  it("edit as a line editor: cursor, backspace, ctrl-w, ctrl-u, home/end, and pastes as one line", async () => {
    const t = await setup(fakeDaemon([item("call dentist")]));
    await t.c.press("i<home>re<end><backspace>t<left><left><ctrl+w>");
    expect((await prompt(t)).line).toEqual({ text: "recall st", cursor: 7 });
    await t.c.press("<end><backspace><backspace>");
    t.c.paste("the\nvet");
    await t.c.idle();
    expect((await prompt(t)).line).toEqual({ text: "recall the vet", cursor: 14 });
    await t.c.press("<left><left><left><ctrl+u>");
    expect((await prompt(t)).line).toEqual({ text: "vet", cursor: 0 });
    await t.c.press("<enter>");
    expect(t.byTitle("vet")).toBeDefined();
  });

  it("does nothing on an empty list", async () => {
    const t = await setup(fakeDaemon([]));
    await t.c.press("i^Dwt@x");
    expect(t.mode().kind).toBe("normal");
  });
});
