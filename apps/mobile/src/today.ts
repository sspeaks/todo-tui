import { useStatusCounts } from "@todo-tui/client/react";
import { dayOf, localDay } from "@todo-tui/present";

/** The daemon's today (what the views count from), else the phone's. */
export function useToday(): number {
  const counts = useStatusCounts();
  return counts.today ? dayOf(counts.today) : localDay(new Date());
}
