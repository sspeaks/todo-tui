// The home screen on the FakeDaemon: tabs, rows, adding with the buttons,
// complete and undo, and opening an item.
import { CreatedVia, Priority, Status } from "@todo-tui/client/portable";
import { fireEvent, screen, waitFor } from "@testing-library/react-native";
import { router } from "expo-router";
import { Home } from "../Home";
import { renderOn, stopStores } from "./render";

afterEach(stopStores);

const items = [
  { id: "01JB0000000000000000000001", title: "buy milk", status: Status.INBOX, labels: ["errand"] },
  { id: "01JB0000000000000000000002", title: "plan trip", status: Status.NEXT },
];

const setup = () => renderOn(items, <Home />);

test("shows the views as tabs and the first one's items", async () => {
  await setup();
  expect(await screen.findByText("buy milk")).toBeTruthy();
  expect(screen.queryByText("plan trip")).toBeNull();
  await fireEvent.press(screen.getByRole("tab", { name: /Next/ }));
  expect(await screen.findByText("plan trip")).toBeTruthy();
});

test("adds with one tap per choice: due, priority, a label in use, a status", async () => {
  const { daemon } = await setup();
  await screen.findByText("buy milk");
  await fireEvent.press(screen.getByLabelText("New todo"));
  await fireEvent.changeText(await screen.findByLabelText("Title"), "call dentist");

  await fireEvent.press(screen.getByLabelText("Due: Today"));
  await fireEvent.press(screen.getByLabelText("Priority +2"));
  await fireEvent.press(screen.getByRole("button", { name: "#errand" }));
  await fireEvent.press(screen.getByRole("button", { name: "Next" }));
  // The chosen one again clears it.
  await fireEvent.press(screen.getByRole("button", { name: "Next" }));
  await fireEvent.press(screen.getByRole("button", { name: "Someday" }));

  // The preview: a tag per field the capture will set.
  expect(await screen.findByLabelText(/^Will add call dentist: \+2, .+, errand, Someday$/)).toBeTruthy();
  await fireEvent.press(screen.getByLabelText("Add"));
  expect(await screen.findByText("Added: call dentist")).toBeTruthy();
  const captured = [...daemon.items.values()].find((i) => i.title === "call dentist");
  expect(captured).toMatchObject({ createdVia: CreatedVia.MOBILE, priority: Priority.HIGH, labels: ["errand"], status: Status.SOMEDAY });
  expect(captured?.dueDate).toBeDefined();
  // The sheet closed, and the + button opens it empty again.
  await waitFor(() => expect(screen.queryByLabelText("Title")).toBeNull());
  await fireEvent.press(screen.getByLabelText("New todo"));
  expect((await screen.findByLabelText("Title")).props.value).toBe("");
});

test("Cancel closes the sheet and drops what was typed", async () => {
  await setup();
  await screen.findByText("buy milk");
  await fireEvent.press(screen.getByLabelText("New todo"));
  await fireEvent.changeText(await screen.findByLabelText("Title"), "never mind");
  await fireEvent.press(screen.getByText("Cancel"));
  await waitFor(() => expect(screen.queryByLabelText("Title")).toBeNull());
  await fireEvent.press(screen.getByLabelText("New todo"));
  expect((await screen.findByLabelText("Title")).props.value).toBe("");
});

test("a repeat preset is one tap", async () => {
  const { daemon } = await setup();
  await screen.findByText("buy milk");
  await fireEvent.press(screen.getByLabelText("New todo"));
  await fireEvent.changeText(await screen.findByLabelText("Title"), "water plants");
  await fireEvent.press(screen.getByLabelText("Repeat Weekdays"));
  expect(await screen.findByLabelText(/^Will add water plants: .*every weekday/)).toBeTruthy();
  await fireEvent.press(screen.getByLabelText("Add"));
  await screen.findByText("Added: water plants");
  const captured = [...daemon.items.values()].find((i) => i.title === "water plants");
  expect(captured?.recurrence?.rule?.kind.case).toBe("weekly");
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
