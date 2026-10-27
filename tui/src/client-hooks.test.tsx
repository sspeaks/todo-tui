// The client's React additions (packages/client/src/hooks.ts), rendered with
// Ink since the client package has no renderer.

import { ClientsProvider, type Item, LiveStoreProvider, Status, type TodoClients, useClients, useItem, useUndoState } from "@todo-tui/client";
import { Text, render } from "ink";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, item } from "./fake.test-util.js";
import { stripAnsi } from "./render.test-util.js";
import { virtualTerminal } from "./term.js";

const cleanups: (() => Promise<void>)[] = [];
afterEach(async () => {
  await Promise.all(cleanups.splice(0).map((c) => c()));
});

function mount(node: React.ReactNode) {
  const term = virtualTerminal(80, 10);
  const app = render(node, term.options);
  cleanups.push(async () => app.unmount());
  return () => stripAnsi(term.stdout.lastFrame());
}

describe("client hooks", () => {
  it("useClients gives the clients from ClientsProvider, and needs one", async () => {
    const fd = fakeDaemon([item("x")]);
    const clients = fd.clients();
    let seen: TodoClients | undefined;
    function Probe() {
      seen = useClients();
      return <Text>ok</Text>;
    }
    const frame = mount(
      <ClientsProvider clients={clients}>
        <Probe />
      </ClientsProvider>,
    );
    await vi.waitFor(() => expect(frame()).toContain("ok"));
    expect(seen).toBe(clients);

    // Outside a provider it throws, which fails the render.
    function Bare() {
      useClients();
      return null;
    }
    const errors = vi.spyOn(console, "error").mockImplementation(() => {});
    try {
      const term = virtualTerminal(80, 10);
      await expect(render(<Bare />, term.options).waitUntilExit()).rejects.toThrow(/no <ClientsProvider>/);
    } finally {
      errors.mockRestore();
    }
  });

  it("useItem with fetch gets an Item no view holds once live, and leaves an unknown id undefined", async () => {
    const fd = fakeDaemon([]);
    const done = fd.put(item("finished", { status: Status.DONE }));
    const store = fd.store();
    cleanups.push(() => store.stop());
    const seen: (Item | undefined)[] = [];
    function Probe({ id, fetch }: { id: string; fetch: boolean }) {
      const it = useItem(id, { fetch });
      seen.push(it);
      return <Text>{it ? it.title : "none"}</Text>;
    }
    const frame = mount(
      <LiveStoreProvider store={store}>
        <Probe id={done.id} fetch={false} />
        <Probe id={done.id} fetch />
        <Probe id="01J00000000000000000000000" fetch />
      </LiveStoreProvider>,
    );
    fd.daemon.down = true;
    store.start();
    await new Promise((r) => setTimeout(r, 50));
    expect(frame()).toBe("none\nnone\nnone");
    // Once the daemon answers and the store is live, the fetch runs; the store then has it for everyone.
    fd.daemon.down = false;
    await vi.waitFor(() => expect(frame()).toBe("finished\nfinished\nnone"));
    const renders = seen.length;
    await new Promise((r) => setTimeout(r, 50));
    expect(seen.length).toBe(renders);
  });

  it("useUndoState follows the undo stack", async () => {
    const a = item("tidy");
    const fd = fakeDaemon([a]);
    const store = fd.store();
    cleanups.push(() => store.stop());
    function Probe() {
      const { status, undo } = useUndoState();
      return <Text>{`${status}:${undo?.description ?? "-"}`}</Text>;
    }
    const frame = mount(
      <LiveStoreProvider store={store}>
        <Probe />
      </LiveStoreProvider>,
    );
    store.start();
    await vi.waitFor(() => expect(frame()).toBe("ready:-"));
    await fd.clients().items.setStatus({ requestId: "01J00000000000000000000001", id: a.id, status: Status.DONE });
    await vi.waitFor(() => expect(frame()).toBe(`ready:mark done "tidy"`));
  });
});
