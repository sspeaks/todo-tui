// Swipe actions, search, settings and adding offline, on the FakeDaemon.
import { CreatedVia, Status } from "@todo-tui/client/portable";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react-native";
import { router } from "expo-router";
import { Alert } from "react-native";
import { Home } from "../Home";
import { SearchScreen } from "../SearchScreen";
import { SettingsScreen } from "../SettingsScreen";
import { renderOn, stopStores } from "./render";

afterEach(stopStores);

const MILK = "01JB0000000000000000000001";
const items = [
  { id: MILK, title: "buy milk", status: Status.INBOX },
  { id: "01JB0000000000000000000002", title: "old milk run", status: Status.DONE },
];

/** Swipes the row showing `title`, through its accessibility action (what VoiceOver offers too). */
const swipe = async (title: string, action: "right" | "left") => {
  await screen.findByText(title);
  const row = screen.getAllByHintText(/^Swipe right/).find((r) => within(r).queryByText(title));
  await fireEvent(row!, "accessibilityAction", { nativeEvent: { actionName: action } });
};

test("swiping right completes, swiping left deletes, each with Undo", async () => {
  const { daemon } = await renderOn(items, <Home />);
  await swipe("buy milk", "right");
  await waitFor(() => expect(daemon.items.get(MILK)?.status).toBe(Status.DONE));
  await fireEvent.press(await screen.findByText("Undo"));
  await waitFor(() => expect(daemon.items.get(MILK)?.status).toBe(Status.INBOX));

  await swipe("buy milk", "left");
  await waitFor(() => expect(daemon.items.get(MILK)?.deletedAt).toBeDefined());
  expect(await screen.findByText("Deleted: buy milk")).toBeTruthy();
  await fireEvent.press(screen.getByText("Undo"));
  await waitFor(() => expect(daemon.items.get(MILK)?.deletedAt).toBeUndefined());
});

test("search finds open and done items as you type", async () => {
  await renderOn(items, <SearchScreen />);
  expect(screen.getByText(/Every item, done and archived/)).toBeTruthy();
  await fireEvent.changeText(screen.getByLabelText("Search"), "mil");
  expect(await screen.findByText("buy milk")).toBeTruthy();
  expect(await screen.findByText("old milk run")).toBeTruthy();
  await fireEvent.changeText(screen.getByLabelText("Search"), "zebra");
  expect(await screen.findByText('Nothing matches "zebra".')).toBeTruthy();
});

test("settings shows the computer, and unpairs after confirming", async () => {
  const { session } = await renderOn(items, <SettingsScreen />, {
    link: { state: "ok", serverInfo: { daemonVersion: "0.1.0", apiMinor: 9, capabilities: [] } as never },
  });
  expect(screen.getByText("mac.tail1.ts.net")).toBeTruthy();
  expect(await screen.findByText("Connected")).toBeTruthy();
  expect(screen.getByText("0.1.0 · API 9")).toBeTruthy();
  jest.spyOn(Alert, "alert").mockImplementation((_t, _m, buttons) => {
    void buttons?.find((b) => b.style === "destructive")?.onPress?.();
  });
  await fireEvent.press(screen.getByText("Unpair this phone"));
  await waitFor(() => expect(session.unpair).toHaveBeenCalled());
  expect(router.replace).toHaveBeenCalledWith("/pair");
});

test("added while the computer is out of reach: kept, shown, then sent once it is back", async () => {
  const { daemon } = await renderOn([], <Home />, { down: true });
  await fireEvent.press(screen.getByLabelText("New todo"));
  await fireEvent.changeText(await screen.findByLabelText("Title"), "call dentist");
  await fireEvent.press(screen.getByLabelText("Priority +2"));
  await fireEvent.press(screen.getByLabelText("Add"));
  expect(await screen.findByText(/Saved on this phone: call dentist/)).toBeTruthy();
  expect(await screen.findByText(/1 waiting to send: call dentist/)).toBeTruthy();
  expect(daemon.items.size).toBe(0);

  await act(async () => {
    daemon.down = false;
  });
  await waitFor(() => expect([...daemon.items.values()].map((i) => i.title)).toEqual(["call dentist"]), { timeout: 3_000 });
  const sent = [...daemon.items.values()][0]!;
  expect(sent.createdVia).toBe(CreatedVia.MOBILE);
  expect(sent.priority).not.toBe(0);
  await waitFor(() => expect(screen.queryByText(/waiting to send/)).toBeNull());
});
