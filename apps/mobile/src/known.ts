// Labels and waiting names already in use, for the pickers' suggestions:
// from the open Items (the default query), which the store keeps live, and
// the People the daemon knows (design 3.5). Most used first, then by name,
// so a short list shows the likely ones.
import { personName } from "@todo-tui/present";
import { usePeople, useView } from "@todo-tui/client/react";
import { useMemo } from "react";

const byUse = (counts: Map<string, number>): string[] =>
  [...counts.keys()].sort((a, b) => counts.get(b)! - counts.get(a)! || a.localeCompare(b));

export function useKnown(): { labels: string[]; names: string[] } {
  const view = useView({});
  const people = usePeople().people;
  return useMemo(() => {
    const labels = new Map<string, number>();
    const names = new Map<string, number>();
    for (const i of view.items) {
      for (const l of i.labels) labels.set(l, (labels.get(l) ?? 0) + 1);
      const n = personName(i.waiting?.on, people);
      if (n && n !== "someone") names.set(n, (names.get(n) ?? 0) + 1);
    }
    // Everyone known, after those waited on.
    for (const p of people.values()) if (p.displayName && !names.has(p.displayName)) names.set(p.displayName, 0);
    return { labels: byUse(labels), names: byUse(names) };
  }, [view.items, people]);
}
