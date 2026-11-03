import { Box, Text } from "ink";
import { singleLine } from "../format/text.js";
import { useUi } from "./hooks.js";

/** :l48's report (design 6.6), as it is pasted, scrolled by j/k and windowed to the body. */
export function ReportPanel({ width, height }: { width: number; height: number }) {
  const mode = useUi((s) => s.mode);
  if (mode.kind !== "report") return null;
  // The daemon strips control characters; the panel draws only what it
  // can draw, as every other place a title shows does (y copies the text).
  const lines = mode.text.trimEnd().split("\n").map(singleLine);
  return (
    <Box flexDirection="column" width={width} height={height} overflow="hidden">
      {lines.slice(mode.scroll, mode.scroll + height).map((line, i) => (
        <Text key={mode.scroll + i} wrap="truncate-end" bold={line === "L48:" || line === "N48:"} color={line === "L48:" || line === "N48:" ? "cyan" : undefined}>
          {line || " "}
        </Text>
      ))}
    </Box>
  );
}
