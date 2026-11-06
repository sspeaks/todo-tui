// Day numbers (days since 1970-01-01, as @todo-tui/present counts them)
// to and from what the daemon and the date picker use.
import type { ProtoDate } from "@todo-tui/client/portable";
import { civil, localDay } from "@todo-tui/present";

/** Day `n` as a google.type.Date. */
export function protoDate(n: number): { year: number; month: number; day: number } {
  const { year, month, day } = civil(n);
  return { year, month, day };
}

/** Day `n` at local midnight, for the date picker. */
export function jsDate(n: number): Date {
  const { year, month, day } = civil(n);
  return new Date(year, month - 1, day);
}

/** The local day a picked Date falls on. */
export const dayOfDate = (d: Date): number => localDay(d);

export type { ProtoDate };
