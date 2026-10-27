import { Box, Text } from "ink";
import { useMemo } from "react";
import { helpLines } from "../keys/help.js";
import { useUi } from "./hooks.js";

/** The help, generated from the keymap, scrolled by j/k and windowed to the body. */
export function HelpOverlay({ width, height }: { width: number; height: number }) {
  const scroll = useUi((s) => (s.mode.kind === "help" ? s.mode.scroll : 0));
  const lines = useMemo(helpLines, []);
  return (
    <Box flexDirection="column" width={width} height={height} overflow="hidden">
      {lines.slice(scroll, scroll + height).map((line, i) => (
        <Text key={scroll + i} wrap="truncate-end" bold={line.kind === "title" || line.kind === "section"} color={line.kind === "section" ? "cyan" : undefined}>
          {line.text || " "}
        </Text>
      ))}
    </Box>
  );
}
