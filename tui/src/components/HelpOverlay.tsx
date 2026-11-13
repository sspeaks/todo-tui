import { Box, Text } from "ink";
import { useMemo } from "react";
import { helpLines, matchRuns, searchWords } from "../keys/help.js";
import { useUi } from "./hooks.js";

/** The help, generated from the keymap, scrolled by j/k and windowed to the body; `/` narrows it, the words it found lit up. */
export function HelpOverlay({ width, height }: { width: number; height: number }) {
  const scroll = useUi((s) => (s.mode.kind === "help" ? s.mode.scroll : 0));
  const query = useUi((s) => (s.mode.kind === "help" ? (s.mode.query ?? "") : ""));
  const typing = useUi((s) => s.mode.kind === "help" && s.mode.line !== undefined);
  const bindings = useUi((s) => s.keymap.bindings);
  const lines = useMemo(() => helpLines(bindings, query, typing), [bindings, query, typing]);
  const words = useMemo(() => searchWords(query), [query]);
  return (
    <Box flexDirection="column" width={width} height={height} overflow="hidden">
      {lines.slice(scroll, scroll + height).map((line, i) => (
        <Text key={scroll + i} wrap="truncate-end" bold={line.kind === "title" || line.kind === "section"} color={line.kind === "section" ? "cyan" : undefined}>
          {(line.kind === "row" || line.kind === "section") && words.length
            ? matchRuns(line.text, words).map((r, j) =>
                r.match ? (
                  <Text key={j} color="yellow" bold>
                    {r.text}
                  </Text>
                ) : (
                  r.text
                ),
              )
            : line.text || " "}
        </Text>
      ))}
    </Box>
  );
}
