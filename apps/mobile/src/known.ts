// Labels and waiting names already in use, for the pickers' suggestions:
// from the open Items (the default query), which the store keeps live.
// Most used first, then by name, so a short list shows the likely ones.
import { personName } from "@todo-tui/present";
import { useView } from "@todo-tui/client/react";
import { useMemo } from "react";

const byUse = (counts: Map<string, number>): string[] =>
  [...counts.keys()].sort((a, b) => counts.get(b)! - counts.get(a)! || a.localeCompare(b));

export function useKnown(): { labels: string[]; names: string[] } {
  const view = useView({});
  return useMemo(() => {
    const labels = new Map<string, number>();
    const names = new Map<string, number>();
    for (const i of view.items) {
      for (const l of i.labels) labels.set(l, (labels.get(l) ?? 0) + 1);
      const n = personName(i.waiting?.on);
      if (n) names.set(n, (names.get(n) ?? 0) + 1);
    }
    return { labels: byUse(labels), names: byUse(names) };
  }, [view.items]);
}
