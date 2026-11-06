// The item screen on the FakeDaemon: each field is its own undoable change.
import { Priority, Status } from "@todo-tui/client/portable";
import { fireEvent, screen, waitFor } from "@testing-library/react-native";
import { router } from "expo-router";
import { Alert } from "react-native";
import { ItemDetail } from "../ItemDetail";
import { renderOn, stopStores } from "./render";

afterEach(stopStores);

const ID = "01JB0000000000000000000001";
const setup = () => renderOn([{ id: ID, title: "buy milk", status: Status.INBOX, labels: ["errand"] }], <ItemDetail id={ID} />);

test("sets the priority, a due date and the status", async () => {
  const { daemon } = await setup();
  await screen.findByDisplayValue("buy milk");

  await fireEvent.press(screen.getByLabelText("Priority +3"));
  expect(await screen.findByText("Priority +3 medium")).toBeTruthy();
  await waitFor(() => expect(daemon.items.get(ID)?.priority).toBe(Priority.MEDIUM));
  // The chosen one again clears it.
  await fireEvent.press(screen.getByLabelText("Priority +3"));
  // No priority: NONE, or unset as FakeDaemon leaves it.
  await waitFor(() => expect([Priority.UNSPECIFIED, Priority.NONE]).toContain(daemon.items.get(ID)?.priority));

  await fireEvent.press(screen.getByLabelText("Due: Tomorrow"));
  await waitFor(() => expect(daemon.items.get(ID)?.dueDate).toBeDefined());

  await fireEvent.press(screen.getByText("Next"));
  await waitFor(() => expect(daemon.items.get(ID)?.status).toBe(Status.NEXT));
  await fireEvent.press(screen.getByText("Undo"));
  await waitFor(() => expect(daemon.items.get(ID)?.status).toBe(Status.INBOX));
});

test("edits the title and saves notes", async () => {
  const { daemon } = await setup();
  const title = await screen.findByDisplayValue("buy milk");
  await fireEvent(title, "focus");
  await fireEvent.changeText(title, "buy oat milk");
  await fireEvent(title, "endEditing");
  await waitFor(() => expect(daemon.items.get(ID)?.title).toBe("buy oat milk"));

  const notes = screen.getByLabelText("Notes");
  await fireEvent(notes, "focus");
  await fireEvent.changeText(notes, "the barista kind");
  await fireEvent.press(await screen.findByText("Save notes"));
  await waitFor(() => expect(daemon.items.get(ID)?.description).toBe("the barista kind"));
  expect(screen.queryByText("Save notes")).toBeNull();
});

test("sets who it waits on, then takes a label off in one tap", async () => {
  const { daemon } = await setup();
  await screen.findByDisplayValue("buy milk");
  await fireEvent.press(screen.getByLabelText("Waiting on someone else"));
  const name = await screen.findByPlaceholderText(/Name/);
  await fireEvent.changeText(name, "the vendor");
  await fireEvent(name, "submitEditing");
  await waitFor(() => expect(daemon.items.get(ID)?.waiting?.on?.ref).toEqual({ case: "freeText", value: "the vendor" }));
  expect(await screen.findByText("Follow up")).toBeTruthy();

  // A name in use is then one tap, and so is taking it off again.
  await fireEvent.press(await screen.findByLabelText("Waiting on the vendor"));
  await waitFor(() => expect(daemon.items.get(ID)?.waiting).toBeUndefined());

  await fireEvent.press(screen.getByRole("button", { name: "#errand" }));
  await waitFor(() => expect(daemon.items.get(ID)?.labels).toEqual([]));
});

test("repeats daily in one tap, and stops in one more", async () => {
  const { daemon } = await setup();
  await screen.findByDisplayValue("buy milk");
  await fireEvent.press(screen.getByLabelText("Repeat Daily"));
  await waitFor(() => expect(daemon.items.get(ID)?.recurrence?.rule?.kind.case).toBe("daily"));
  expect(await screen.findByText(/^every day/)).toBeTruthy();
  await fireEvent.press(screen.getByLabelText("Repeat Daily"));
  await waitFor(() => expect(daemon.items.get(ID)?.recurrence?.rule).toBeUndefined());
});

test("deletes after confirming, and goes back", async () => {
  const { daemon } = await setup();
  await screen.findByDisplayValue("buy milk");
  jest.spyOn(Alert, "alert").mockImplementation((_title, _msg, buttons) => {
    void buttons?.find((b) => b.style === "destructive")?.onPress?.();
  });
  await fireEvent.press(screen.getByText("Delete"));
  await waitFor(() => expect(daemon.items.get(ID)?.deletedAt).toBeDefined());
  expect(router.back).toHaveBeenCalled();
});
