import { useEffect, useRef, useState } from "react";

/** Ids that appeared after the first load, each kept for `ms` milliseconds. */
export function useFreshIds(ids: readonly string[], ready: boolean, ms = 1500): ReadonlySet<string> {
  const known = useRef<Set<string> | null>(null);
  const timers = useRef(new Set<ReturnType<typeof setTimeout>>());
  const [fresh, setFresh] = useState<ReadonlySet<string>>(new Set());

  useEffect(() => {
    if (!ready) return;
    if (!known.current) {
      known.current = new Set(ids);
      return;
    }
    const seen = known.current;
    const added = ids.filter((id) => !seen.has(id));
    if (!added.length) return;
    for (const id of added) seen.add(id);
    setFresh((prev) => new Set([...prev, ...added]));
    const t = setTimeout(() => {
      timers.current.delete(t);
      setFresh((prev) => new Set([...prev].filter((id) => !added.includes(id))));
    }, ms);
    timers.current.add(t);
  }, [ids, ready, ms]);

  useEffect(() => () => timers.current.forEach(clearTimeout), []);
  return fresh;
}
