// Renders a screen on the client's in-memory FakeDaemon with what the root
// layout gives it: a ready session, the store, the clients, the snackbar
// and the outbox.
import { FakeDaemon } from "@todo-tui/client/fake";
import { type ItemSchema, LiveStore, type MessageInitShape, type StoreCache, type Transport, createClients } from "@todo-tui/client/portable";
import { ClientsProvider, LiveStoreProvider } from "@todo-tui/client/react";
import { act, render } from "@testing-library/react-native";
import type { ReactElement } from "react";
import { OutboxProvider } from "../../outbox";
import { type Link, type SessionApi, SessionContext } from "../../session";
import { SnackProvider } from "../../snack";

const stores: LiveStore[] = [];

export async function stopStores(): Promise<void> {
  await act(async () => {
    for (const s of stores.splice(0)) await s.stop();
  });
}

export const URL = "https://mac.tail1.ts.net";

/**
 * `daemon` instead of a new one on `items`; `cache`, what the store shows
 * until it loads (as at a launch); `transport`, the store's way to the
 * daemon (one that holds its calls, say).
 */
export async function renderOn(
  items: MessageInitShape<typeof ItemSchema>[],
  ui: ReactElement,
  opts: { link?: Link; down?: boolean; daemon?: FakeDaemon; cache?: StoreCache; transport?: Transport } = {},
) {
  const daemon = opts.daemon ?? new FakeDaemon(items);
  daemon.down = opts.down ?? false;
  const store = new LiveStore({ transport: opts.transport ?? daemon.transport, debounceMs: 0, backoff: { initialMs: 10, maxMs: 20 }, ...(opts.cache && { cache: opts.cache }) });
  stores.push(store);
  store.start();
  const clients = createClients(daemon.transport);
  const session: SessionApi = {
    session: {
      state: "ready",
      pairing: { url: URL, token: "todo_t" },
      conn: { ...clients, transport: daemon.transport },
      store,
      link: opts.link ?? { state: "checking" },
    },
    pair: jest.fn(async () => {}),
    unpair: jest.fn(async () => {}),
    retry: jest.fn(),
  };
  await render(
    <SessionContext.Provider value={session}>
      <LiveStoreProvider store={store}>
        <ClientsProvider clients={clients}>
          <SnackProvider>
            <OutboxProvider url={URL}>{ui}</OutboxProvider>
          </SnackProvider>
        </ClientsProvider>
      </LiveStoreProvider>
    </SessionContext.Provider>,
  );
  return { daemon, store, session };
}
