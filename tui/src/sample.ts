import { Priority, Status, type Item } from "@todo-tui/client";

const row = (id: string, title: string, status: Status, effectivePriority: Priority) =>
  ({ id, title, status, effectivePriority }) as Item;

/** A fixed list for --smoke; only the fields ItemList reads are set. */
export const sampleItems = (): Item[] => [
  row("01", "Call dentist", Status.INBOX, Priority.MEDIUM),
  row("02", "Fix flaky login integration test that times out on CI under load", Status.IN_PROGRESS, Priority.HIGHEST),
  row("03", "Renew passport", Status.DONE, Priority.NONE),
  row("04", "Old idea", Status.CANCELLED, Priority.LOWEST),
];
