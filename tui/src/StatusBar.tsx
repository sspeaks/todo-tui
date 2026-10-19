import type { ConnectionSnapshot, ConnectionState } from "@todo-tui/client";
import { Box, Text } from "ink";

const indicator: Record<ConnectionState, [string, string]> = {
  idle: ["○", "gray"],
  connecting: ["◌", "yellow"],
  live: ["●", "green"],
  reconnecting: ["◌", "yellow"],
  closed: ["○", "red"],
};

const describe = (err: unknown) => (err instanceof Error ? err.message : String(err));

export function StatusBar({
  connection,
  count,
  viewError,
}: {
  connection: ConnectionSnapshot;
  count: number;
  viewError?: unknown;
}) {
  const { state, revision, error } = connection;
  const [glyph, color] = indicator[state];
  const shown = state !== "live" && error !== undefined ? error : viewError;
  const detail = shown !== undefined ? ` · ${describe(shown)}` : "";
  return (
    <Box>
      <Box flexGrow={1} flexShrink={1}>
        <Text wrap="truncate-end">
          <Text color={color}>
            {glyph} {state}
          </Text>
          <Text dimColor>
            {" "}
            · rev {revision.toString()} · {count} items{detail}
          </Text>
        </Text>
      </Box>
      <Text dimColor> j/k move · q quit</Text>
    </Box>
  );
}
