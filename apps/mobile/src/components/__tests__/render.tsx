// Renders a screen on the client's in-memory FakeDaemon, with the store,
// the clients and the snackbar, as the root layout gives them.
import { FakeDaemon } from "@todo-tui/client/fake";
import { type ItemSchema, LiveStore, type MessageInitShape, createClients } from "@todo-tui/client/portable";
import { ClientsProvider, LiveStoreProvider } from "@todo-tui/client/react";
import { act, render } from "@testing-library/react-native";
import type { ReactElement } from "react";
import { SnackProvider } from "../../snack";

let stores: LiveStore[] = [];

export async function stopStores(): Promise<void> {
  await act(async () => {
    for (const s of stores.splice(0)) await s.stop();
  });
}

export async function renderOn(items: MessageInitShape<typeof ItemSchema>[], ui: ReactElement) {
  const daemon = new FakeDaemon(items);
  const store = new LiveStore({ transport: daemon.transport, debounceMs: 0 });
  stores.push(store);
  store.start();
  await render(
    <LiveStoreProvider store={store}>
      <ClientsProvider clients={createClients(daemon.transport)}>
        <SnackProvider>{ui}</SnackProvider>
      </ClientsProvider>
    </LiveStoreProvider>,
  );
  return { daemon, store };
}
