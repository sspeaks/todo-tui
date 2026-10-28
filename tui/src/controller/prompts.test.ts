import { Status, setStatus, updateItem } from "@todo-tui/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, item, rows } from "../fake.test-util.js";
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
    expect((await prompt(t)).people).toEqual(["Dana Scully", "dave"]);
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
    await t.c.press("i^Dwtx");
    expect(t.mode().kind).toBe("normal");
  });
});
