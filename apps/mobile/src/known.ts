// Labels and waiting names already in use, for the pickers' suggestions:
// from the open Items (the default query), which the store keeps live.
import { personName } from "@todo-tui/present";
import { useView } from "@todo-tui/client/react";
import { useMemo } from "react";

export function useKnown(): { labels: string[]; names: string[] } {
  const view = useView({});
  return useMemo(() => {
    const labels = new Set<string>();
    const names = new Set<string>();
    for (const i of view.items) {
      for (const l of i.labels) labels.add(l);
      const n = personName(i.waiting?.on);
      if (n) names.add(n);
    }
    return { labels: [...labels].sort(), names: [...names].sort() };
  }, [view.items]);
}
