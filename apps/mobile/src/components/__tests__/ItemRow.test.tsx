// The list's rows: the priority as the row's coloured edge, the status
// mark (shown at once on a tap, taken back if the change fails), the mono
// meta line and the due date.
import { ItemSchema, Priority, Status, create } from "@todo-tui/client/portable";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react-native";
import { LayoutAnimation, StyleSheet } from "react-native";
import { Home } from "../Home";
import { LINGER_MS } from "../ItemList";
import { ItemRow } from "../ItemRow";
import { renderOn, stopStores } from "./render";

afterEach(stopStores);

const now = new Date();
const today = { year: now.getFullYear(), month: now.getMonth() + 1, day: now.getDate() };

const items = [
  {
    id: "01JB0000000000000000000001",
    title: "call the vendor",
    status: Status.INBOX,
    priority: Priority.HIGHEST,
    labels: ["work"],
    waiting: { on: { ref: { case: "freeText" as const, value: "dana" } } },
    dueDate: today,
  },
  { id: "01JB0000000000000000000002", title: "buy milk", status: Status.INBOX },
];

const colorOf = (el: { props: { style?: unknown } }) => (StyleSheet.flatten(el.props.style as never) as { backgroundColor?: string; color?: string });

test("shows the priority as the row's coloured edge, none without one", async () => {
  await renderOn(items, <Home />);
  await screen.findByText("buy milk");
  const edges = screen.getAllByTestId("priority-edge");
  expect(edges).toHaveLength(1);
  // +1, in the light theme (Jest's colour scheme).
  expect(colorOf(edges[0]!).backgroundColor).toBe("#ef4444");
});

test("reads the row's details aloud, and shows waiting, labels and a due date of today in its columns", async () => {
  await renderOn(items, <Home />);
  expect(await screen.findByLabelText("call the vendor, priority +1 highest, due Today, waiting on dana, #work")).toBeTruthy();
  expect(screen.getByText("#work")).toBeTruthy();
  expect(screen.getByText("dana")).toBeTruthy();
  expect(colorOf(screen.getByText("^Today")).color).toBe("#0c6b85");
});

test("takes the mark back when the daemon refuses the change", async () => {
  const { daemon } = await renderOn(items, <Home />);
  await screen.findByText("buy milk");
  expect(screen.getAllByTestId("status-inbox")).toHaveLength(2);
  daemon.down = true;
  await act(async () => {
    fireEvent.press(screen.getAllByLabelText("Complete")[1]!);
  });
  // Back to open once the daemon refused it, with why on the snackbar.
  await waitFor(() => expect(screen.getAllByTestId("status-inbox")).toHaveLength(2));
  expect(screen.queryByTestId("status-done")).toBeNull();
});

test("shows the mark done at once on a tap, until the change lands or fails", async () => {
  const item = create(ItemSchema, { id: "01JB0000000000000000000003", title: "water plants", status: Status.NEXT });
  let answer: (ok: boolean) => void = () => {};
  const onToggle = jest.fn(() => new Promise<boolean>((resolve) => (answer = resolve)));
  const ui = (i: typeof item) => <ItemRow item={i} today={0} onToggle={onToggle} onOpen={jest.fn()} />;
  const { rerender } = await render(ui(item));
  await fireEvent.press(screen.getByLabelText("Complete"));
  expect(screen.getByTestId("status-done")).toBeTruthy();
  expect(screen.getByLabelText("Reopen")).toBeTruthy();
  // Refused: open again.
  await act(async () => answer(false));
  expect(screen.getByTestId("status-next")).toBeTruthy();
  // Done: it stays shown done, and then the Item itself says so.
  await fireEvent.press(screen.getByLabelText("Complete"));
  await act(async () => answer(true));
  expect(screen.getByTestId("status-done")).toBeTruthy();
  await rerender(ui(create(ItemSchema, { ...item, status: Status.DONE })));
  expect(screen.getByTestId("status-done")).toBeTruthy();
  // Undone (the snackbar's Undo): open, not the tap's done again.
  await rerender(ui(item));
  expect(screen.getByTestId("status-next")).toBeTruthy();
  // Reopened elsewhere: the Item's status wins over the tap's.
  await rerender(ui(create(ItemSchema, { ...item, status: Status.IN_PROGRESS })));
  expect(screen.getByTestId("status-in_progress")).toBeTruthy();
  expect(onToggle).toHaveBeenCalledTimes(2);
});

test("sends nothing for a second tap while the first is on its way", async () => {
  const item = create(ItemSchema, { id: "01JB0000000000000000000004", title: "feed the cat", status: Status.NEXT });
  let answer: (ok: boolean) => void = () => {};
  const onToggle = jest.fn(() => new Promise<boolean>((resolve) => (answer = resolve)));
  await render(<ItemRow item={item} today={0} onToggle={onToggle} onOpen={jest.fn()} />);
  await fireEvent.press(screen.getByLabelText("Complete"));
  await fireEvent.press(screen.getByLabelText("Reopen"));
  expect(onToggle).toHaveBeenCalledTimes(1);
  await act(async () => answer(true));
});

test("keeps a row completed with a tap a beat, checked, then lets it go", async () => {
  const { daemon } = await renderOn(items, <Home />);
  await screen.findByText("buy milk");
  const started = Date.now();
  await act(async () => {
    fireEvent.press(screen.getAllByLabelText("Complete")[1]!);
  });
  // Done on the daemon, and still listed, checked.
  await waitFor(() => expect(daemon.items.get("01JB0000000000000000000002")?.status).toBe(Status.DONE));
  expect(screen.getByText("buy milk")).toBeTruthy();
  expect(screen.getByTestId("status-done")).toBeTruthy();
  await waitFor(() => expect(screen.queryByText("buy milk")).toBeNull(), { timeout: LINGER_MS + 2_000 });
  expect(Date.now() - started).toBeGreaterThanOrEqual(LINGER_MS - 50);
});

test("animates a row leaving, but not the first rows a view shows or a tab switch", async () => {
  const animate = jest.spyOn(LayoutAnimation, "configureNext");
  try {
    await renderOn(items, <Home />);
    await screen.findByText("buy milk");
    await fireEvent.press(screen.getByRole("tab", { name: /^Next/ }));
    await fireEvent.press(screen.getByRole("tab", { name: /^Inbox/ }));
    await screen.findByText("buy milk");
    expect(animate).not.toHaveBeenCalled();
    await act(async () => {
      fireEvent.press(screen.getAllByLabelText("Complete")[1]!);
    });
    await waitFor(() => expect(screen.queryByText("buy milk")).toBeNull(), { timeout: LINGER_MS + 2_000 });
    expect(animate).toHaveBeenCalledTimes(1);
  } finally {
    animate.mockRestore();
  }
});
