import { Box, Text } from "ink";
import type { Item } from "@todo-tui/client";
import { priorityLabel, singleLine, statusGlyph } from "./format.js";

interface Props {
  items: readonly Item[];
  selectedId: string | undefined;
  fresh: ReadonlySet<string>;
  offset: number;
  height: number;
}

export function ItemList({ items, selectedId, fresh, offset, height }: Props) {
  const rows = items.slice(offset, offset + height);
  return (
    <Box flexDirection="column" height={height}>
      {rows.map((item) => {
        const selected = item.id === selectedId;
        const isFresh = fresh.has(item.id);
        return (
          <Text
            key={item.id}
            wrap="truncate-end"
            inverse={selected}
            color={isFresh ? "green" : undefined}
            bold={isFresh}
          >
            {selected ? "›" : " "} {statusGlyph(item.status)} {priorityLabel(item).padEnd(3)} {singleLine(item.title)}
          </Text>
        );
      })}
    </Box>
  );
}
