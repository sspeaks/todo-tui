import { useStatusCounts, useViews } from "@todo-tui/client";
import { Box, Text } from "ink";
import { singleLine } from "../format/text.js";
import { TAB_SEPARATOR, tabLabels } from "../state/layout.js";
import { useUi } from "./hooks.js";

const describe = (err: unknown) => (err instanceof Error ? err.message : String(err));

/** The views as tabs, with their counts (zero counts left out); shortened under width pressure. */
export function TabBar({ width }: { width: number }) {
  const { views, status, error } = useViews();
  const counts = useStatusCounts();
  const tab = useUi((s) => s.tab);
  if (!views.length) {
    return (
      <Box width={width} height={1}>
        <Text dimColor wrap="truncate-end">
          {status === "error" ? `Views: ${singleLine(describe(error))}` : "Loading views…"}
        </Text>
      </Box>
    );
  }
  const labels = tabLabels(
    views.map((v) => ({ id: v.id, name: singleLine(v.name) })),
    counts.views,
    tab,
    width,
  );
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
