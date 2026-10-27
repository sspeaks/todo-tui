import { useStatusCounts, useViews } from "@todo-tui/client";
import { Box, Text } from "ink";
import { useMemo } from "react";
import { singleLine } from "../format/text.js";
import { TAB_SEPARATOR, tabLabels } from "../state/layout.js";
import { searchTab, tabQuery } from "../state/tabs.js";
import { useOptionalView, useUi } from "./hooks.js";

const describe = (err: unknown) => (err instanceof Error ? err.message : String(err));

/**
 * The views as tabs, with their counts (zero counts left out), then the
 * Search tab while open, counting its rows (never `/`'s); shortened under
 * width pressure.
 */
export function TabBar({ width }: { width: number }) {
  const { views, status, error } = useViews();
  const counts = useStatusCounts();
  const tab = useUi((s) => s.tab);
  const query = useUi((s) => s.search?.query);
  const search = useOptionalView(query === undefined ? undefined : tabQuery(searchTab(query)));
  const found = search?.status === "ready" ? search.items.length : 0;
  const all = useMemo(() => {
    const tabs = views.map((v) => ({ id: v.id, name: singleLine(v.name) }));
    return query === undefined ? tabs : [...tabs, { id: searchTab(query), name: `Search: ${singleLine(query)}`, numbered: false }];
  }, [views, query]);
  const tabCounts = useMemo(
    () => (query === undefined ? counts.views : new Map(counts.views).set(searchTab(query), found)),
    [counts.views, query, found],
  );
  if (!views.length) {
    return (
      <Box width={width} height={1}>
        <Text dimColor wrap="truncate-end">
          {status === "error" ? `Views: ${singleLine(describe(error))}` : "Loading views…"}
        </Text>
      </Box>
    );
  }
  const labels = tabLabels(all, tabCounts, tab, width);
  return (
    <Box width={width} height={1}>
      <Text wrap="truncate-end">
        {labels.map((l, i) => (
          <Text key={l.id}>
            {i ? <Text dimColor>{TAB_SEPARATOR}</Text> : null}
            <Text inverse={l.current} bold={l.current}>
              {l.text}
            </Text>
          </Text>
        ))}
      </Text>
    </Box>
  );
}
