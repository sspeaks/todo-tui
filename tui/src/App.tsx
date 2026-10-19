import { useConnection, useView } from "@todo-tui/client";
import { Box, Text, useApp, useInput, useWindowSize } from "ink";
import { useEffect, useMemo, useRef, useState } from "react";
import { inboxQuery } from "./inbox.js";
import { ItemList } from "./ItemList.js";
import { StatusBar } from "./StatusBar.js";
import { useFreshIds } from "./useFreshIds.js";

const clamp = (n: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, n));

/** The Inbox list; expects a started LiveStore from <LiveStoreProvider>. */
export function App() {
  const { items, status, error: viewError } = useView(inboxQuery);
  const connection = useConnection();
  const { exit } = useApp();
  const { rows } = useWindowSize();
  const height = Math.max(1, (rows || 24) - 2);

  const [selectedId, setSelectedId] = useState<string>();
  const lastIndex = useRef(0);
  const offset = useRef(0);

  let index = selectedId === undefined ? -1 : items.findIndex((i) => i.id === selectedId);
  if (index < 0) index = clamp(lastIndex.current, 0, items.length - 1);
  const selected = items[index];
  lastIndex.current = Math.max(index, 0);
  offset.current = clamp(offset.current, index - height + 1, index);
  offset.current = clamp(offset.current, 0, Math.max(0, items.length - height));

  // Pin the fallback selection so later inserts don't shift it.
  useEffect(() => {
    if (selected && selected.id !== selectedId) setSelectedId(selected.id);
  }, [selected, selectedId]);

  const ids = useMemo(() => items.map((i) => i.id), [items]);
  const fresh = useFreshIds(ids, status === "ready");

  useInput((input, key) => {
    if (input === "q" || (key.ctrl && input === "c")) {
      exit();
      return;
    }
    const step = input === "j" || key.downArrow ? 1 : input === "k" || key.upArrow ? -1 : 0;
    const next = items[clamp(index + step, 0, items.length - 1)];
    if (step && next) setSelectedId(next.id);
  });

  return (
    <Box flexDirection="column">
      <Text bold>
        Inbox <Text dimColor>{items.length}</Text>
      </Text>
      {status === "ready" && !items.length ? (
        <Box height={height}>
          <Text dimColor>Inbox is empty.</Text>
        </Box>
      ) : (
        <ItemList items={items} selectedId={selected?.id} fresh={fresh} offset={offset.current} height={height} />
      )}
      <StatusBar connection={connection} count={items.length} viewError={viewError} />
    </Box>
  );
}
