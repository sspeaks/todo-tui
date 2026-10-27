import { Code, ConnectError, CreatedVia, type ItemsClient, MAX_CAPTURE_BYTES, Priority, Status, deleteItem } from "@todo-tui/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, item, rows } from "../fake.test-util.js";
import { parseKeys } from "../keys/tokenize.js";
import { typedLines } from "./quick-add.js";
import { holding, type Setup, setup, stopControllers } from "./setup.test-util.js";

afterEach(async () => {
  await stopControllers();
  vi.useRealTimers();
});

/** Monday 2026-10-26, noon UTC (the tests run with TZ=UTC). */
const NOON = new Date(Date.UTC(2026, 9, 26, 12));
const clock = () => NOON;
const dated = (init = rows(2)) => fakeDaemon(init, { now: clock, timeZone: "UTC" });

/** Quick-add's mode, once the preview has caught up with the text. */
async function quickAdd(t: Setup) {
  await t.c.previewIdle();
  const mode = t.mode();
  if (mode.kind !== "quickAdd") throw new Error(`not quick-add: ${mode.kind}`);
  return mode;
}

/** The Items captured, oldest first. */
const captured = (t: Setup) =>
  t.userOps()
    .filter((o) => o.summary.startsWith("capture "))
    .map((o) => t.daemon.items.get(o.events[0]?.itemId ?? ""));

describe("quick-add", () => {
  it("previews as it is typed, adds on enter (created via the TUI), stays open, and closes on enter when empty", async () => {
    const t = await setup(dated(), { now: clock });
    await t.c.press("a");
    expect(t.mode()).toEqual({ kind: "quickAdd", line: { text: "", cursor: 0 } });
    await t.c.press("call ^fri +2 #home");
    const mode = await quickAdd(t);
    expect(mode.preview?.text).toBe("call ^fri +2 #home");
    expect(mode.preview?.draft).toMatchObject({ title: "call", priority: Priority.HIGH, dueDate: { year: 2026, month: 10, day: 30 }, labels: ["home"] });
    await t.c.press("<enter>");
    expect(t.byTitle("call")).toMatchObject({ priority: Priority.HIGH, dueDate: { day: 30 }, labels: ["home"], createdVia: CreatedVia.TUI });
    expect(t.mode()).toMatchObject({ kind: "quickAdd", line: { text: "" } });
    // It shows in this tab: no toast, and the selection stays.
    expect(t.titles()).toEqual(["row 0", "row 1", "call"]);
    expect(t.toast()).toBeUndefined();
    expect(t.selected()).toBe("row 0");
    await t.c.press("<enter>");
    expect(t.mode().kind).toBe("normal");
  });

  it("esc discards; ctrl-c cancels and keeps the app (hard case 9)", async () => {
    const t = await setup(dated());
    await t.c.press("amilk<esc>");
    expect(t.mode().kind).toBe("normal");
    await t.c.press("abread<ctrl+c>");
    expect(t.mode().kind).toBe("normal");
    expect(t.quit).not.toHaveBeenCalled();
    expect(captured(t)).toEqual([]);
    // In normal mode, ctrl-c quits.
    await t.c.press("<ctrl+c>");
    expect(t.quit).toHaveBeenCalledTimes(1);
  });

  it("captures each line of a fast-typed burst, in order, and stays open (hard case 2)", async () => {
    const t = await setup(dated());
    await t.c.press("a");
    t.c.input("milk\rbread\r", {});
    await t.c.idle();
    expect(captured(t).map((i) => i?.title)).toEqual(["milk", "bread"]);
    expect(t.mode()).toMatchObject({ kind: "quickAdd", line: { text: "" } });
    expect(t.titles()).toEqual(["row 0", "row 1", "milk", "bread"]);
  });

  it("captures each non-blank line of a paste, in order, with what was typed before it (hard case 8)", async () => {
    const t = await setup(dated());
    await t.c.press("abuy ");
    t.c.paste("milk\n\n  \nbread +1\r\neggs\n");
    await t.c.idle();
    expect(captured(t).map((i) => i?.title)).toEqual(["buy milk", "bread", "eggs"]);
    expect(t.byTitle("bread")?.priority).toBe(Priority.HIGHEST);
    expect(t.mode()).toMatchObject({ kind: "quickAdd", line: { text: "" } });
    // One line goes in at the cursor, uncaptured.
    t.c.paste("cheese");
    await t.c.idle();
    expect(t.mode()).toMatchObject({ kind: "quickAdd", line: { text: "cheese" } });
  });

  it("refuses text over 64 KiB with the text kept, and never sends it (hard case 44)", async () => {
    const t = await setup(dated());
    await t.c.press("a");
    const big = "x".repeat(MAX_CAPTURE_BYTES + 1);
    t.c.paste(big);
    await t.c.press("<enter>");
    expect(t.toast()).toBe(`Too long to capture: ${(MAX_CAPTURE_BYTES + 1).toLocaleString("en")} bytes, at most ${MAX_CAPTURE_BYTES.toLocaleString("en")}`);
    const mode = await quickAdd(t);
    expect(mode.line.text).toBe(big);
    expect(mode.preview).toBeUndefined();
    expect(captured(t)).toEqual([]);
    // Back under the limit, it captures.
    await t.c.press("<backspace>");
    await t.c.press("<enter>");
    expect(captured(t)).toHaveLength(1);
  });

  it("keeps one ParseCapture in flight, sends the latest text after it, and never shows a stale preview (hard case 42)", async () => {
    const h = holding(["parseCapture"]);
    const sent: string[] = [];
    const t = await setup(dated(), {
      items: (items) => {
        const held = h.wrap(items);
        return new Proxy(held, {
          get(target, prop, receiver) {
            const value = Reflect.get(target, prop, receiver);
            if (prop !== "parseCapture") return value;
            return (req: { text: string }, opts: unknown) => {
              sent.push(req.text);
              return (value as (r: unknown, o: unknown) => unknown)(req, opts);
            };
          },
        });
      },
    });
    await t.c.press("a");
    h.hold();
    for (const ch of "call +2") await t.c.press(ch === " " ? "<space>" : ch);
    // Only the first keystroke's request went out; the text has moved on.
    expect(sent).toEqual(["c"]);
    h.release();
    const mode = await quickAdd(t);
    expect(sent).toEqual(["c", "call +2"]);
    expect(mode.preview?.text).toBe("call +2");
    expect(mode.preview?.draft.priority).toBe(Priority.HIGH);
  });

  it("inserts a burst of characters as one run, with one preview request", async () => {
    const sent: string[] = [];
    const t = await setup(dated(), {
      items: (items) =>
        new Proxy(items, {
          get: (target, prop, receiver) =>
            prop === "parseCapture"
              ? (req: { text: string }, opts: never) => {
                  sent.push(req.text);
                  return target.parseCapture(req as never, opts);
                }
              : Reflect.get(target, prop, receiver),
        }),
    });
    await t.c.press("a");
    t.c.input("call dentist +2", {});
    await t.c.idle();
    expect((await quickAdd(t)).preview?.text).toBe("call dentist +2");
    expect(sent).toEqual(["call dentist +2"]);
    // A long unbracketed paste arrives as keys: still one insert, at once.
    const started = Date.now();
    t.c.input("y".repeat(MAX_CAPTURE_BYTES), {});
    await t.c.idle();
    expect(Date.now() - started).toBeLessThan(1_000);
    expect((await quickAdd(t)).line.text).toHaveLength(15 + MAX_CAPTURE_BYTES);
  });

  it("drops a preview answering text since edited", async () => {
    const h = holding(["parseCapture"]);
    const t = await setup(dated(), { items: h.wrap });
    await t.c.press("a");
    h.hold();
    await t.c.press("x");
    await t.c.press("<backspace>");
    h.release();
    // "x"'s answer came while the line was empty: not shown; "" asks nothing.
    const mode = await quickAdd(t);
    expect(mode.line.text).toBe("");
    expect(mode.preview).toBeUndefined();
  });

  it("says where a capture went when this tab won't show it", async () => {
    const t = await setup(dated([item("n", { status: Status.NEXT })]));
    await t.c.press("3");
    await vi.waitFor(() => expect(t.titles()).toEqual(["n"]));
    await t.c.press("amilk<enter>");
    expect(t.toast()).toBe('capture "milk" → Inbox');
    await t.c.press("bread >next<enter>");
    expect(t.toast()).toBe('capture "milk" → Inbox');
    expect(t.titles()).toEqual(["n", "bread"]);
    await t.c.press("cheese >someday<enter>");
    expect(t.toast()).toBe('capture "cheese" → Someday');
  });

  it("keeps the text when a capture fails, and the lines typed behind it", async () => {
    let fail = true;
    const t = await setup(dated(), {
      items: (items) =>
        new Proxy(items, {
          get: (target, prop, receiver) =>
            prop === "captureItem" && fail
              ? async () => {
                  fail = false;
                  throw new ConnectError("boom", Code.Internal);
                }
              : Reflect.get(target, prop, receiver),
        }),
    });
    await t.c.press("a");
    t.c.input("milk\rbread\rjam", {});
    await t.c.idle();
    expect(t.toast()).toBe("Internal: boom");
    expect(await quickAdd(t)).toMatchObject({ line: { text: "milk" }, rest: ["bread", "jam"] });
    await t.c.press("<enter>");
    expect(await quickAdd(t)).toMatchObject({ line: { text: "bread" }, rest: ["jam"] });
    await t.c.press("<enter><enter>");
    expect(captured(t).map((i) => i?.title)).toEqual(["milk", "bread", "jam"]);
    expect(await quickAdd(t)).toMatchObject({ line: { text: "" } });
  });

  it("waits for the daemon and keeps the text when it stays down", async () => {
    const t = await setup(dated(), { timing: { retryMs: 200 } });
    await t.c.press("amilk");
    t.daemon.down = true;
    await t.c.press("<enter>");
    expect(t.toast()).toBe("Not saved: daemon unavailable");
    expect(t.mode()).toMatchObject({ kind: "quickAdd", line: { text: "milk" } });
    t.daemon.down = false;
    await t.c.press("<enter>");
    expect(captured(t).map((i) => i?.title)).toEqual(["milk"]);
  });
});

describe("quick-add: no capture lands twice", () => {
  /** A client whose captures reach the daemon, but whose answers are lost while `lose` says so; it records the request_ids. */
  function losing() {
    const ids: string[] = [];
    const state = { lose: true };
    return {
      ids,
      state,
      wrap: (items: ItemsClient): ItemsClient =>
        new Proxy(items, {
          get: (target, prop, receiver) =>
            prop === "captureItem"
              ? async (req: { requestId: string }, opts: never) => {
                  ids.push(req.requestId);
                  const res = await target.captureItem(req as never, opts);
                  if (state.lose) throw new ConnectError("deadline", Code.DeadlineExceeded);
                  return res;
                }
              : Reflect.get(target, prop, receiver),
        }),
    };
  }

  it("sends the same request_id again for the same text after an unknown outcome, a new one for new text", async () => {
    const l = losing();
    const t = await setup(dated(), { items: l.wrap });
    await t.c.press("amilk<enter>");
    expect(t.toast()).toBe("The daemon did not answer; the change may not be saved");
    expect(await quickAdd(t)).toMatchObject({ line: { text: "milk" } });
    // It did land; enter again replays it rather than adding a second.
    l.state.lose = false;
    await t.c.press("<enter>");
    expect(l.ids).toHaveLength(2);
    expect(l.ids[1]).toBe(l.ids[0]);
    expect(captured(t).map((i) => i?.title)).toEqual(["milk"]);
    expect(await quickAdd(t)).toMatchObject({ line: { text: "" } });
    // After a success, the same text again is a new capture.
    await t.c.press("milk<enter>");
    expect(l.ids[2]).not.toBe(l.ids[1]);
    expect(captured(t).map((i) => i?.title)).toEqual(["milk", "milk"]);
    // Text changed after an unknown outcome: a fresh id.
    l.state.lose = true;
    await t.c.press("bread<enter>");
    l.state.lose = false;
    await t.c.press("<backspace>s<enter>");
    expect(l.ids[4]).not.toBe(l.ids[3]);
    expect(captured(t).map((i) => i?.title)).toEqual(["milk", "milk", "bread", "breas"]);
  });

  it("also after the daemon stayed down past the retry window", async () => {
    const l = losing();
    l.state.lose = false;
    const t = await setup(dated(), { timing: { retryMs: 200 }, items: l.wrap });
    await t.c.press("amilk");
    t.daemon.down = true;
    await t.c.press("<enter>");
    expect(t.toast()).toBe("Not saved: daemon unavailable");
    t.daemon.down = false;
    await t.c.press("<enter>");
    expect(new Set(l.ids).size).toBe(1);
    expect(captured(t).map((i) => i?.title)).toEqual(["milk"]);
  });
});

describe("A", () => {
  it("captures a subtask of the selected Item, named above the line", async () => {
    const t = await setup(dated(rows(2)));
    await t.c.press("jA");
    const parent = t.byTitle("row 1")!;
    expect(t.mode()).toMatchObject({ kind: "quickAdd", parentId: parent.id, parentTitle: "row 1" });
    await t.c.press("step one<enter>step two<enter><esc>");
    expect(captured(t).map((i) => [i?.title, i?.parentId])).toEqual([
      ["step one", parent.id],
      ["step two", parent.id],
    ]);
    // a, after it, adds at the top level.
    await t.c.press("aloose<enter><esc>");
    expect(t.byTitle("loose")?.parentId).toBeUndefined();
  });

  it("keeps the text and says why when the parent was deleted while typing (hard case 43)", async () => {
    const t = await setup(dated(rows(2)));
    await t.c.press("A");
    const parent = t.byTitle("row 0")!;
    await t.c.press("child");
    await deleteItem(t.clients().items, parent.id);
    await t.c.press("<enter>");
    expect(t.state().toast).toMatchObject({ level: "error", text: "The parent was deleted — esc, then a, to add at the top level" });
    expect(await quickAdd(t)).toMatchObject({ line: { text: "child" }, parentId: parent.id });
    expect(captured(t)).toEqual([]);
    await t.c.press("<esc>a");
    expect(t.mode()).toMatchObject({ kind: "quickAdd", line: { text: "" } });
    expect(t.mode()).not.toHaveProperty("parentId");
  });
});

describe("typedLines", () => {
  it("replays queued keys and pastes into lines, up to an esc", () => {
    expect(typedLines(parseKeys("ab<enter>c<backspace>d"))).toEqual({ lines: ["ab", "d"], dropped: 0 });
    expect(typedLines([...parseKeys("x"), { paste: "y\nz" }, ...parseKeys("<enter><enter>w<esc>jj")])).toEqual({ lines: ["xy", "z", "w"], dropped: 3 });
  });
});
