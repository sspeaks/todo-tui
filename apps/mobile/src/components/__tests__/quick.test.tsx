// The add sheet's short lists: the most used labels and names, what is
// chosen always, and "More…" for the rest.
import { render, screen } from "@testing-library/react-native";
import { QuickLabels } from "../quick/QuickLabels";
import { QuickWaiting } from "../quick/QuickWaiting";

const labels = ["work", "home", "errand", "health", "money", "car", "garden"];

test("labels: the five most used, what is chosen, and More… for the rest", async () => {
  await render(<QuickLabels value={["garden"]} known={labels} onChange={() => {}} limit={5} />);
  const shown = screen.getAllByRole("button").map((b) => b.props.accessibilityLabel);
  expect(shown).toEqual(["#garden", "#work", "#home", "#errand", "#health", "#money", "More labels"]);
});

test("labels: no More… when every one fits", async () => {
  await render(<QuickLabels value={[]} known={labels.slice(0, 3)} onChange={() => {}} limit={5} />);
  expect(screen.getByLabelText("New label")).toBeTruthy();
});

test("names: the chosen one first, then the most used", async () => {
  await render(<QuickWaiting value="zoe" known={["dana", "the vendor", "sam", "ali", "kim", "lee"]} onChange={() => {}} limit={5} />);
  const shown = screen.getAllByRole("button").map((b) => b.props.accessibilityLabel);
  expect(shown).toEqual([
    "Waiting on zoe",
    "Waiting on dana",
    "Waiting on the vendor",
    "Waiting on sam",
    "Waiting on ali",
    "Waiting on kim",
    "Waiting on someone else",
  ]);
  expect(screen.getByText("More…")).toBeTruthy();
});
