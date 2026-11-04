// The home screen on the FakeDaemon: tabs, rows, adding with the buttons,
// complete and undo, and opening an item.
import { CreatedVia, Priority, Status } from "@todo-tui/client/portable";
import { fireEvent, screen, waitFor } from "@testing-library/react-native";
import { router } from "expo-router";
import { Home } from "../Home";
import { renderOn, stopStores } from "./render";

afterEach(stopStores);

const items = [
  { id: "01JB0000000000000000000001", title: "buy milk", status: Status.INBOX },
  { id: "01JB0000000000000000000002", title: "plan trip", status: Status.NEXT },
];

const setup = () => renderOn(items, <Home onUnpair={jest.fn(async () => {})} />);

test("shows the views as tabs and the first one's items", async () => {
  await setup();
  expect(await screen.findByText("buy milk")).toBeTruthy();
  expect(screen.queryByText("plan trip")).toBeNull();
  await fireEvent.press(screen.getByRole("tab", { name: /Next/ }));
  expect(await screen.findByText("plan trip")).toBeTruthy();
});

test("adds with the buttons: a priority and a new label become one MOBILE capture", async () => {
  const { daemon } = await setup();
  await screen.findByText("buy milk");
  await fireEvent.changeText(screen.getByPlaceholderText("Add a todo"), "call dentist");

  await fireEvent.press(screen.getByText("Priority"));
  await fireEvent.press(await screen.findByText("High"));
  expect(await screen.findByText("+2 High")).toBeTruthy();

  await fireEvent.press(screen.getByText("# Labels"));
  await fireEvent.changeText(await screen.findByPlaceholderText("New label"), "home");
  await fireEvent(screen.getByPlaceholderText("New label"), "submitEditing");
  await fireEvent.press(screen.getByText("Done"));
  expect(await screen.findByText("#home")).toBeTruthy();

  expect(await screen.findByText("→ call dentist · +2 · #home")).toBeTruthy();
  await fireEvent.press(screen.getByLabelText("Add"));
  expect(await screen.findByText("Added: call dentist")).toBeTruthy();
  expect(await screen.findByText("+2 call dentist")).toBeTruthy();
  const captured = [...daemon.items.values()].find((i) => i.title === "call dentist");
  expect(captured).toMatchObject({ createdVia: CreatedVia.MOBILE, priority: Priority.HIGH, labels: ["home"] });
  // The buttons reset for the next one.
  expect(screen.getByText("Priority")).toBeTruthy();
});

test("completes an item and undoes it from the snackbar", async () => {
  await setup();
  await screen.findByText("buy milk");
  await fireEvent.press(screen.getByLabelText("Complete"));
  expect(await screen.findByText("Done: buy milk")).toBeTruthy();
  await waitFor(() => expect(screen.queryByText("buy milk")).toBeNull());
  await fireEvent.press(screen.getByText("Undo"));
  expect(await screen.findByText(/^Undid: /)).toBeTruthy();
  expect(await screen.findByText("buy milk")).toBeTruthy();
});

test("a row opens its item", async () => {
  await setup();
  await fireEvent.press(await screen.findByText("buy milk"));
  expect(router.push).toHaveBeenCalledWith({ pathname: "/item/[id]", params: { id: "01JB0000000000000000000001" } });
});
