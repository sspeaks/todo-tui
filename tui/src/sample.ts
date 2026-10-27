import { type ItemSchema, type MessageInitShape, Priority, Status, timestampFromDate } from "@todo-tui/client";
import { localDay } from "./format/dates.js";

type ItemInit = MessageInitShape<typeof ItemSchema>;

/** A google.type.Date `days` after the local date of `now`. */
function after(now: Date, days: number) {
  const d = new Date((localDay(now) + days) * 86_400_000);
  return { year: d.getUTCFullYear(), month: d.getUTCMonth() + 1, day: d.getUTCDate() };
}

/** Items for --smoke: every list column in use, dates relative to `now`. */
export function sampleItems(now: Date): ItemInit[] {
  const at = (minutes: number) => timestampFromDate(new Date(now.getTime() - minutes * 60_000));
  return [
    { title: "Call dentist", priority: Priority.MEDIUM, dueDate: after(now, 3), labels: ["health"], createdAt: at(50) },
    {
      title: "Fix flaky login integration test that times out on CI under load",
      status: Status.IN_PROGRESS,
      priority: Priority.HIGHEST,
      dueDate: after(now, -1),
      createdAt: at(40),
    },
    { title: "Renew passport", dueDate: after(now, 30), createdAt: at(30), description: "Photos first.\nThen the form." },
    {
      title: "Contract from the vendor",
      waiting: { on: { ref: { case: "freeText", value: "dana" } }, since: after(now, -3), followUpOn: after(now, 0) },
      createdAt: at(20),
    },
    { title: "Old idea", status: Status.SOMEDAY, priority: Priority.LOWEST, createdAt: at(10) },
  ];
}
