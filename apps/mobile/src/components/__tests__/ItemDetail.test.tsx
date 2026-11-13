// The item screen on the FakeDaemon: each field is its own undoable change.
import { FakeDaemon } from "@todo-tui/client/fake";
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

test("groups the fields in cards, with the status mark by the title, every status in sight, and the parent under Info", async () => {
  await renderOn(
    [
      { id: "01JB0000000000000000000009", title: "plan trip", status: Status.NEXT },
      { id: ID, title: "buy milk", status: Status.DONE, parentId: "01JB0000000000000000000009" },
    ],
    <ItemDetail id={ID} />,
  );
  await screen.findByDisplayValue("buy milk");
  for (const caption of ["Status", "Priority", "When", "People & labels", "Notes", "Info"]) expect(screen.getByText(caption)).toBeTruthy();
  expect(screen.getByTestId("status-done")).toBeTruthy();
  for (const status of ["Inbox", "Next", "Someday", "Doing", "Done", "Cancelled"]) expect(screen.getByRole("button", { name: status })).toBeTruthy();
  // The parent opens from Info.
  await fireEvent.press(await screen.findByText("plan trip"));
  expect(router.push).toHaveBeenCalledWith({ pathname: "/item/[id]", params: { id: "01JB0000000000000000000009" } });
});

test("names a person by their name, offers the People known, and saves one by id", async () => {
  const daemon = new FakeDaemon([
    {
      id: ID,
      title: "buy milk",
      status: Status.INBOX,
      remoteLink: { trackerKind: "jira", instanceId: "work-jira", remoteId: "1", key: "PLAT-1", url: "https://jira.example.com/browse/PLAT-1" },
    },
  ]);
  const dana = daemon.putPerson({ displayName: "Dana Example", emails: ["dana@example.com"] });
  const item = daemon.items.get(ID)!;
  item.assignee = { $typeName: "todo.v1.PersonRef", ref: { case: "personId", value: dana.id } };
  await renderOn([], <ItemDetail id={ID} />, { daemon });
  await screen.findByDisplayValue("buy milk");
  // The assignee, and a pill to wait on her: by name, never her id.
  expect(await screen.findAllByText("Dana Example")).toHaveLength(2);
  expect(screen.queryByText(dana.id)).toBeNull();
  await fireEvent.press(await screen.findByLabelText("Waiting on Dana Example"));
  await waitFor(() => expect(daemon.items.get(ID)?.waiting?.on?.ref).toEqual({ case: "personId", value: dana.id }));
});
