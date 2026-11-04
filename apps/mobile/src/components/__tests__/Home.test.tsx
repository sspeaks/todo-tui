// The home screen against the client's in-memory FakeDaemon: tabs, rows,
// quick-add with its preview, complete and undo.
import { FakeDaemon } from "@todo-tui/client/fake";
import { CreatedVia, type LiveStore, LiveStore as Store, Status, createClients } from "@todo-tui/client/portable";
import { ClientsProvider, LiveStoreProvider } from "@todo-tui/client/react";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react-native";
import { Home } from "../Home";

jest.mock("expo-router", () => ({ Stack: { Screen: () => null } }));
jest.mock("react-native-safe-area-context", () => jest.requireActual<{ default: unknown }>("react-native-safe-area-context/jest/mock").default);

let store: LiveStore | undefined;
afterEach(async () => {
  await act(async () => {
    await store?.stop();
  });
  store = undefined;
});

async function setup() {
  const daemon = new FakeDaemon([
    { id: "01JB0000000000000000000001", title: "buy milk", status: Status.INBOX },
    { id: "01JB0000000000000000000002", title: "plan trip", status: Status.NEXT },
  ]);
  store = new Store({ transport: daemon.transport, debounceMs: 0 });
  store.start();
  const onUnpair = jest.fn(async () => {});
  await render(
    <LiveStoreProvider store={store}>
      <ClientsProvider clients={createClients(daemon.transport)}>
        <Home onUnpair={onUnpair} />
      </ClientsProvider>
    </LiveStoreProvider>,
  );
  return { daemon, onUnpair };
}

test("shows the views as tabs and the first one's items", async () => {
  await setup();
  expect(await screen.findByText("buy milk")).toBeTruthy();
  expect(screen.getByRole("tab", { name: /Inbox/ })).toBeTruthy();
  expect(screen.queryByText("plan trip")).toBeNull();
  await fireEvent.press(screen.getByRole("tab", { name: /Next/ }));
  expect(await screen.findByText("plan trip")).toBeTruthy();
});

test("quick-add previews the capture, then adds it as MOBILE", async () => {
  const { daemon } = await setup();
  await screen.findByText("buy milk");
  await fireEvent.changeText(screen.getByPlaceholderText(/call dentist/), "call dentist +2 #home");
  expect(await screen.findByText(/→ call dentist · \+2 · #home/)).toBeTruthy();
  await fireEvent.press(screen.getByLabelText("Add"));
  expect(await screen.findByText("Added: call dentist")).toBeTruthy();
  expect(await screen.findByText("+2 call dentist")).toBeTruthy();
  const captured = [...daemon.items.values()].find((i) => i.title === "call dentist");
  expect(captured?.createdVia).toBe(CreatedVia.MOBILE);
});

test("completes an item and undoes it", async () => {
  await setup();
  await screen.findByText("buy milk");
  await fireEvent.press(screen.getByLabelText("Complete"));
  expect(await screen.findByText("Done: buy milk")).toBeTruthy();
  await waitFor(() => expect(screen.queryByText("buy milk")).toBeNull());
  await fireEvent.press(screen.getByText("Undo"));
  expect(await screen.findByText(/^Undid: /)).toBeTruthy();
  expect(await screen.findByText("buy milk")).toBeTruthy();
});
