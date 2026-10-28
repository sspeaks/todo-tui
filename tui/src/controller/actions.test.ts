import { Code, ConnectError, OpKind, Priority, Status, captureItem, CreatedVia, deleteItem, setStatus, updateItem } from "@todo-tui/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, item, rows } from "../fake.test-util.js";
import { holding, setup, stopControllers } from "./setup.test-util.js";

afterEach(async () => {
  await stopControllers();
  vi.useRealTimers();
});

const statusOf = (t: Awaited<ReturnType<typeof setup>>, title: string) => t.byTitle(title)?.status;

describe("x X and the pipeline", () => {
  it("x takes the row out at once, selects the next one at the same index, and toasts the op (hard case 10)", async () => {
    const t = await setup(fakeDaemon(rows(5)));
    // No waiting: the row is gone when the keys are handled.
    await t.c.press("jjx");
    expect(statusOf(t, "row 2")).toBe(Status.DONE);
    expect(t.titles()).toEqual(["row 0", "row 1", "row 3", "row 4"]);
    expect(t.selected()).toBe("row 3");
    expect(t.list().anchor).toBe(2);
    expect(t.toast()).toBe('mark done "row 2" · u undo');
    expect(t.store.undoState.getSnapshot().undo?.description).toBe('mark done "row 2"');
  });

  it("xx in one chunk completes two different Items, two ops (hard case 11)", async () => {
    const t = await setup(fakeDaemon(rows(5)));
    t.c.input("xx", {});
    await t.c.idle();
    expect(t.userOps().map((o) => o.summary)).toEqual(['mark done "row 0"', 'mark done "row 1"']);
    expect(t.titles()).toEqual(["row 2", "row 3", "row 4"]);
    expect(t.selected()).toBe("row 2");
    // A third and a fourth x in quick succession, as presses.
    await Promise.all([t.c.press("x"), t.c.press("x")]);
    expect(t.userOps()).toHaveLength(4);
    expect(t.titles()).toEqual(["row 4"]);
  });

  it("x on the last row selects the new last row; on the only row leaves none, and the next row in is selected (hard case 12)", async () => {
    const t = await setup(fakeDaemon(rows(3)));
    await t.c.press("Gx");
    expect(t.selected()).toBe("row 1");
    await t.c.press("xx");
    expect(t.titles()).toEqual([]);
    expect(t.c.selectedId()).toBeUndefined();
    // x on nothing does nothing.
    await t.c.press("x");
    expect(t.userOps()).toHaveLength(3);
    const fresh = item("fresh");
    t.put(fresh);
    await vi.waitFor(() => expect(t.c.selectedId()).toBe(fresh.id));
  });

  it("does not toggle: x on a done Item, X on a cancelled one, say so and send nothing", async () => {
    const t = await setup(fakeDaemon([item("done one", { status: Status.DONE }), item("cancelled one", { status: Status.CANCELLED })]));
    await t.c.press("6");
    await vi.waitFor(() => expect(t.titles()).toHaveLength(2));
    await t.c.press(t.selected() === "done one" ? "x" : "jx");
    expect(t.toast()).toBe("already done; s to reopen");
    await t.c.press(t.selected() === "cancelled one" ? "X" : "kX");
    expect(t.toast()).toBe("already cancelled; s to reopen");
    expect(t.userOps()).toEqual([]);
  });

  it("words the toast itself, without u, when another op lands on top first", async () => {
    const fd = fakeDaemon(rows(3));
    const t = await setup(fd, {
      items: (items) =>
        new Proxy(items, {
          get(target, prop, receiver) {
            if (prop !== "setStatus") return Reflect.get(target, prop, receiver);
            return async (req: never, opts: never) => {
              const res = await target.setStatus(req, opts);
              await captureItem(target, "meanwhile", { createdVia: CreatedVia.CLI });
              return res;
            };
          },
        }),
    });
    await t.c.press("x");
    expect(statusOf(t, "row 0")).toBe(Status.DONE);
    expect(t.toast()).toBe('mark done "row 0"');
    expect(t.store.undoState.getSnapshot().undo?.description).toBe('capture "meanwhile"');
  });

  it("X cancels", async () => {
    const t = await setup(fakeDaemon(rows(2)));
    await t.c.press("X");
    expect(statusOf(t, "row 0")).toBe(Status.CANCELLED);
    expect(t.toast()).toBe('cancel "row 0" · u undo');
  });

  it("queues every key behind an action in flight, shows saving… after a while, and runs them in order once it settles (hard case 36)", async () => {
    const h = holding(["setStatus"]);
    const t = await setup(fakeDaemon(rows(5)), { items: h.wrap, timing: { savingMs: 20 } });
    h.hold();
    t.c.input("xj", {});
    await vi.waitFor(() => expect(h.held.count).toBe(1));
    // j waits: the list still has row 0, and nothing moved.
    expect(t.selected()).toBe("row 0");
    await vi.waitFor(() => expect(t.state().busy).toBe("saving…"));
    h.release();
    await t.c.idle();
    expect(t.state().busy).toBeUndefined();
    // j ran on the settled list: row 0 is gone, so it moved from row 1 to row 2.
    expect(t.titles()).toEqual(["row 1", "row 2", "row 3", "row 4"]);
    expect(t.selected()).toBe("row 2");
  });

  it("drops the item keys queued behind an action that failed, and says how many (hard case 36)", async () => {
    let fail = true;
    const t = await setup(fakeDaemon(rows(5)), {
      items: (items) =>
        new Proxy(items, {
          get: (target, prop, receiver) =>
            prop === "setStatus" && fail
              ? async () => {
                  throw new ConnectError("deadline", Code.DeadlineExceeded);
                }
              : Reflect.get(target, prop, receiver),
        }),
    });
    t.c.input("xjx", {});
    await t.c.idle();
    // The j before the second x only moved: it ran. The x did not.
    expect(t.toast()).toBe("The daemon did not answer; the change may not be saved · 1 key dropped");
    expect(t.state().toast?.level).toBe("error");
    expect(t.selected()).toBe("row 1");
    expect(t.titles()).toEqual(["row 0", "row 1", "row 2", "row 3", "row 4"]);
    fail = false;
    // The error stays until the next key, which runs.
    await t.c.press("j");
    expect(t.state().toast).toBeUndefined();
    expect(t.selected()).toBe("row 2");
  });

  it("says “X” was deleted elsewhere when the Item went while the action was in flight", async () => {
    const h = holding(["setStatus"]);
    const t = await setup(fakeDaemon(rows(3)), { items: h.wrap });
    h.hold();
    t.c.input("x", {});
    await vi.waitFor(() => expect(h.held.count).toBe(1));
    await deleteItem(t.clients().items, t.byTitle("row 0")!.id);
    h.release();
    await t.c.idle();
    expect(t.toast()).toBe("“row 0” was deleted elsewhere");
    expect(t.state().toast?.level).toBe("error");
  });

  it("gives up after the live wait with the daemon down, and navigation still works (hard case 37)", async () => {
    const t = await setup(fakeDaemon(rows(3)), { timing: { retryMs: 300 } });
    t.daemon.down = true;
    const started = Date.now();
    await t.c.press("x");
    expect(Date.now() - started).toBeGreaterThanOrEqual(250);
    expect(t.toast()).toBe("Not saved: daemon unavailable");
    expect(t.userOps()).toEqual([]);
    await t.c.press("j");
    expect(t.selected()).toBe("row 1");
  });

  it("runs the navigation keys typed while an action waits out an outage, and drops only from the first that acts (Decision 6)", async () => {
    const t = await setup(fakeDaemon(rows(5)), { timing: { retryMs: 300 } });
    t.daemon.down = true;
    // j, then gg G (a prefix and a move), then dd and the j after it.
    t.c.input("xjggGkddj", {});
    await t.c.idle();
    expect(t.toast()).toBe("Not saved: daemon unavailable · 3 keys dropped");
    expect(t.selected()).toBe("row 3");
    expect(t.state().mode).toEqual({ kind: "normal", pending: [] });
    expect(t.userOps()).toEqual([]);
  });

  it("retries a call cut by a daemon restart with the same request_id, once live again, and it applies once (hard case 38)", async () => {
    const requestIds: string[] = [];
    let cut = true;
    const fd = fakeDaemon(rows(3));
    const t = await setup(fd, {
      items: (items) =>
        new Proxy(items, {
          get(target, prop, receiver) {
            if (prop !== "setStatus") return Reflect.get(target, prop, receiver);
            return async (req: { requestId: string }, opts: unknown) => {
              requestIds.push(req.requestId);
              const res = await target.setStatus(req as never, opts as never);
              if (cut) {
                // It committed, then the daemon went down before the response.
                cut = false;
                fd.daemon.down = true;
                fd.dropStreams();
                setTimeout(() => (fd.daemon.down = false), 50);
                throw new ConnectError("connection reset", Code.Unavailable);
              }
              return res;
            };
          },
        }),
    });
    const id = t.c.selectedId();
    await t.c.press("x");
    expect(requestIds).toHaveLength(2);
    expect(requestIds[1]).toBe(requestIds[0]);
    expect(t.userOps().filter((o) => o.kind === OpKind.SET_STATUS)).toHaveLength(1);
    expect(t.store.getItem(id!)?.status).toBe(Status.DONE);
    expect(t.toast()).toBe('mark done "row 0" · u undo');
    expect(t.store.getConnection().state).toBe("live");
    expect(t.titles()).toEqual(["row 1", "row 2"]);
    expect(t.selected()).toBe("row 1");
  });

  it("quits at once with nothing in flight, else once the action settles, waiting at most a second", async () => {
    const h = holding(["setStatus"]);
    const t = await setup(fakeDaemon(rows(3)), { items: h.wrap, timing: { quitMs: 150 } });
    h.hold();
    t.c.input("x", {});
    await vi.waitFor(() => expect(h.held.count).toBe(1));
    t.c.input("q", {});
    expect(t.state().quitting).toBe(true);
    expect(t.quit).not.toHaveBeenCalled();
    setTimeout(() => h.release(), 30);
    await vi.waitFor(() => expect(t.quit).toHaveBeenCalledTimes(1));
    expect(statusOf(t, "row 0")).toBe(Status.DONE);

    const h2 = holding(["setStatus"]);
    const t2 = await setup(fakeDaemon(rows(3)), { items: h2.wrap, timing: { quitMs: 100 } });
    h2.hold();
    t2.c.input("x", {});
    await vi.waitFor(() => expect(h2.held.count).toBe(1));
    const started = Date.now();
    t2.c.input("c", { ctrl: true });
    await vi.waitFor(() => expect(t2.quit).toHaveBeenCalledTimes(1));
    expect(Date.now() - started).toBeGreaterThanOrEqual(90);
    h2.release();
  });
});

describe("s p z W", () => {
  it("s opens a picker on the current status: a letter picks, j/k and enter pick, esc cancels", async () => {
    const t = await setup(fakeDaemon(rows(3)));
    await t.c.press("s");
    expect(t.mode()).toMatchObject({ kind: "picker", index: 0 });
    await t.c.press("<esc>");
    expect(t.mode().kind).toBe("normal");
    await t.c.press("sn");
    expect(statusOf(t, "row 0")).toBe(Status.NEXT);
    expect(t.toast()).toBe('move to next "row 0" · u undo');
    expect(t.selected()).toBe("row 1");
    await t.c.press("sjj");
    expect(t.mode()).toMatchObject({ kind: "picker", index: 2 });
    await t.c.press("k<enter>");
    expect(statusOf(t, "row 1")).toBe(Status.NEXT);
    // The current status picked again: nothing sent.
    await t.c.press("s<enter>");
    expect(t.toast()).toBe("already inbox");
    expect(t.userOps()).toHaveLength(2);
    // Keys a picker does not know do nothing.
    await t.c.press("sq");
    expect(t.mode().kind).toBe("picker");
    expect(t.quit).not.toHaveBeenCalled();
    await t.c.press("p");
    expect(statusOf(t, "row 2")).toBe(Status.IN_PROGRESS);
  });

  it("p 1–5 and p 0 set the priority without a toast; p 9 and p esc send nothing (hard case 5)", async () => {
    const t = await setup(fakeDaemon(rows(3)));
    await t.c.press("p9");
    expect(t.toast()).toBe("p 9: not bound · ? for help");
    await t.c.press("p<esc>");
    expect(t.mode()).toEqual({ kind: "normal", pending: [] });
    expect(t.userOps()).toEqual([]);
    await t.c.press("p2");
    expect(t.byTitle("row 0")?.priority).toBe(Priority.HIGH);
    expect(t.toast()).toBe("p 9: not bound · ? for help");
    await t.c.press("p0");
    expect(t.byTitle("row 0")?.priority).toBe(Priority.NONE);
    expect(t.userOps().map((o) => o.summary)).toEqual(['edit "row 0"', 'edit "row 0"']);
  });

  it("p that moves the Item out of the tab toasts, and the selection follows it within the tab (hard case 15)", async () => {
    const fd = fakeDaemon([item("a", { status: Status.NEXT }), item("b", { status: Status.NEXT }), item("c", { status: Status.NEXT })]);
    const t = await setup(fd);
    await t.c.press("3");
    await vi.waitFor(() => expect(t.titles()).toEqual(["a", "b", "c"]));
    await t.c.press("jjp1");
    // Next sorts by priority: c moves to the top, and stays selected.
    expect(t.titles()).toEqual(["c", "a", "b"]);
    expect(t.selected()).toBe("c");
    expect(t.list().offset).toBe(0);
  });

  it("z archives with a toast, and unarchives in Archive", async () => {
    const t = await setup(fakeDaemon(rows(2)));
    await t.c.press("z");
    expect(t.byTitle("row 0")?.archived).toBeDefined();
    expect(t.toast()).toBe('archive "row 0" · u undo');
    expect(t.titles()).toEqual(["row 1"]);
    await t.c.press("6");
    await vi.waitFor(() => expect(t.titles()).toEqual(["row 0"]));
    await t.c.press("z");
    expect(t.byTitle("row 0")?.archived).toBeUndefined();
    expect(t.toast()).toBe('unarchive "row 0" · u undo');
  });

  it("W clears waiting; on an Item not waiting it says so", async () => {
    const fd = fakeDaemon([item("vendor", { status: Status.NEXT, waiting: { on: { ref: { case: "freeText", value: "dana" } }, since: { year: 2026, month: 10, day: 20 } } })]);
    const t = await setup(fd);
    await t.c.press("4");
    await vi.waitFor(() => expect(t.titles()).toEqual(["vendor"]));
    await t.c.press("W");
    expect(t.byTitle("vendor")?.waiting).toBeUndefined();
    // It left the Waiting tab: toasted.
    expect(t.toast()).toBe('edit "vendor" · u undo');
    await t.c.press("3");
    await vi.waitFor(() => expect(t.titles()).toEqual(["vendor"]));
    await t.c.press("W");
    expect(t.toast()).toBe("Not waiting on anyone");
  });
});

describe("dd", () => {
  async function family() {
    const before = item("before");
    const parent = item("parent");
    const fd = fakeDaemon([before, parent, item("child one", { parentId: parent.id }), item("child two", { parentId: parent.id }), item("after")]);
    return { t: await setup(fd), parent };
  }

  it("confirms with the subtask count; y deletes the three, selects the same index and toasts the op (hard case 30)", async () => {
    const { t } = await family();
    await t.c.press("jdd");
    expect(t.mode()).toMatchObject({ kind: "confirm", text: 'Delete "parent" and its 2 subtasks? y/N' });
    await t.c.press("y");
    expect(t.mode().kind).toBe("normal");
    expect(t.titles()).toEqual(["before", "after"]);
    expect(t.selected()).toBe("after");
    expect(t.toast()).toBe('delete "parent" and 2 subtasks · u undo');
    for (const title of ["parent", "child one", "child two"]) expect(t.byTitle(title)?.deletedAt).toBeDefined();
  });

  it("u brings all three back and selects the parent (hard case 31)", async () => {
    const { t } = await family();
    await t.c.press("jddy");
    await t.c.press("u");
    expect(t.toast()).toBe('Undid: delete "parent" and 2 subtasks');
    expect(t.titles()).toEqual(["before", "parent", "child one", "child two", "after"]);
    expect(t.selected()).toBe("parent");
  });

  it("any key but y cancels, and is swallowed (hard case 32)", async () => {
    const { t } = await family();
    for (const key of ["n", "<esc>", "j", "Y", "<enter>"]) {
      await t.c.press(`dd${key}`);
      expect(t.mode(), key).toEqual({ kind: "normal", pending: [] });
      expect(t.selected(), key).toBe("before");
    }
    expect(t.userOps()).toEqual([]);
    expect(t.titles()).toHaveLength(5);
  });

  it("dd on a single Item names it alone; ddy in one chunk deletes it", async () => {
    const t = await setup(fakeDaemon(rows(2)));
    t.c.input("ddy", {});
    await t.c.idle();
    expect(t.byTitle("row 0")?.deletedAt).toBeDefined();
    expect(t.toast()).toBe('delete "row 0" · u undo');
  });
});

describe("u and ctrl-r", () => {
  it("undoes what was just done, even typed straight after (hard case 27)", async () => {
    const t = await setup(fakeDaemon(rows(2)));
    t.c.input("a", {});
    t.c.input("buy milk\r", {});
    t.c.input("", { escape: true });
    t.c.input("u", {});
    await t.c.idle();
    expect(t.toast()).toBe('Undid: capture "buy milk"');
    expect(t.byTitle("buy milk")?.deletedAt).toBeDefined();
    expect(t.titles()).toEqual(["row 0", "row 1"]);
  });

  it("undoes another client's op the hint names; one landing unseen fails safe (hard case 28)", async () => {
    const t = await setup(fakeDaemon(rows(2)));
    await t.c.press("x");
    const items = t.clients().items;
    await captureItem(items, "from the cli", { createdVia: CreatedVia.CLI });
    await vi.waitFor(() => expect(t.store.undoState.getSnapshot().undo?.description).toBe('capture "from the cli"'));
    await t.c.press("u");
    expect(t.toast()).toBe('Undid: capture "from the cli"');
    expect(t.byTitle("from the cli")?.deletedAt).toBeDefined();
    expect(statusOf(t, "row 0")).toBe(Status.DONE);

    // The hint names "mark done"; the CLI's capture lands before u is read.
    const t2 = await setup(fakeDaemon(rows(2)), { store: { debounceMs: 60_000 } });
    await t2.c.press("x");
    expect(t2.store.undoState.getSnapshot().undo?.description).toBe('mark done "row 0"');
    await captureItem(t2.clients().items, "sneaky", { createdVia: CreatedVia.CLI });
    await t2.c.press("u");
    expect(t2.toast()).toBe('Undo stack changed — u now: capture "sneaky"');
    expect(t2.state().toast?.level).toBe("error");
    expect(statusOf(t2, "row 0")).toBe(Status.DONE);
    expect(t2.byTitle("sneaky")?.deletedAt).toBeUndefined();
    // Now the hint is right, and u undoes the capture.
    await t2.c.press("u");
    expect(t2.toast()).toBe('Undid: capture "sneaky"');
  });

  it("says what was kept, redoes, and says when there is nothing (hard case 29)", async () => {
    const t = await setup(fakeDaemon([item("X")]));
    await t.c.press("u");
    expect(t.toast()).toBe("Nothing to undo");
    await t.c.press("<ctrl+r>");
    expect(t.toast()).toBe("Nothing to redo");
    const id = t.byTitle("X")!.id;
    // Another client edits two fields as one op; then something changes one of them again, off the stack.
    await updateItem(t.clients().items, { id, title: "Y", priority: Priority.HIGH }, ["title", "priority"]);
    t.daemon.update(id, { priority: Priority.LOW });
    await vi.waitFor(() => expect(t.store.undoState.getSnapshot().undo?.description).toBe('edit "Y"'));
    await t.c.press("u");
    expect(t.toast()).toBe('Undid: edit "Y" · 1 change kept (changed since)');
    expect(t.byTitle("X")?.priority).toBe(Priority.LOW);
    await t.c.press("<ctrl+r>");
    expect(t.toast()).toBe('Redid: edit "Y"');
    expect(t.byTitle("Y")).toBeDefined();
  });

  it("could not undo an entry whose every change has changed since", async () => {
    const t = await setup(fakeDaemon([item("X")]));
    await t.c.press("p1");
    t.daemon.update(t.byTitle("X")!.id, { priority: Priority.LOW });
    await vi.waitFor(() => expect(t.store.getItem(t.byTitle("X")!.id)?.priority).toBe(Priority.LOW));
    await t.c.press("u");
    expect(t.toast()).toBe('Could not undo: edit "X" (everything it changed has changed since)');
  });

  it("selects the Item an undo brings back into the tab", async () => {
    const t = await setup(fakeDaemon(rows(4)));
    await t.c.press("jjx");
    expect(t.selected()).toBe("row 3");
    await t.c.press("gg");
    await t.c.press("u");
    expect(t.toast()).toBe('Undid: mark done "row 2"');
    expect(t.selected()).toBe("row 2");
    expect(statusOf(t, "row 2")).toBe(Status.INBOX);
  });
});

describe("errors", () => {
  it("shows an invalid argument with its field, and keeps the prompt", async () => {
    const t = await setup(fakeDaemon(rows(1)));
    await t.c.press("i<ctrl+u>");
    t.c.paste("x".repeat(70_000));
    await t.c.press("<enter>");
    expect(t.toast()).toMatch(/^Invalid title: /);
    expect(t.mode()).toMatchObject({ kind: "prompt", prompt: "title" });
    expect(t.byTitle("row 0")).toBeDefined();
  });

  it("an unknown error shows its code and message", async () => {
    const t = await setup(fakeDaemon(rows(1)), {
      items: (items) =>
        new Proxy(items, {
          get: (target, prop, receiver) =>
            prop === "archiveItem"
              ? async () => {
                  throw new ConnectError("disk on fire", Code.Internal);
                }
              : Reflect.get(target, prop, receiver),
        }),
    });
    await t.c.press("z");
    expect(t.toast()).toBe("Internal: disk on fire");
  });

  it("another client's change never toasts, and a background status change does not move the selection's screen (hard case 14)", async () => {
    const t = await setup(fakeDaemon(rows(4)));
    await t.c.press("j");
    await setStatus(t.clients().items, t.c.selectedId()!, Status.DONE);
    await vi.waitFor(() => expect(t.selected()).toBe("row 2"));
    expect(t.toast()).toBeUndefined();
  });
});
