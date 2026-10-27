import { MAX_CAPTURE_BYTES } from "@todo-tui/client";
import { Box, Text } from "ink";
import { captureBytes } from "../controller/quick-add.js";
import { captureSummary, lineSegments, tokenSpans } from "../format/capture.js";
import { singleLine, truncate } from "../format/text.js";
import { useToday, useUi } from "./hooks.js";

const LABEL = "Add: ";
/** The byte counter shows from here on. */
const COUNTER_FROM = Math.floor(MAX_CAPTURE_BYTES * 0.9);

const HINT = "+1–5 priority · ^date due · #label · ~person waiting · >next >someday · enter adds · esc closes";

/**
 * Quick-add (Decision 7), at the top of the list: the line being typed,
 * its tokens coloured by kind (red when not applied), and below it what
 * enter would capture, with ParseCapture's warnings. The preview shows
 * only while it answers the text as typed.
 */
export function QuickAddBar({ width }: { width: number }) {
  const mode = useUi((s) => (s.mode.kind === "quickAdd" ? s.mode : undefined));
  const today = useToday();
  if (!mode) return null;
  const { line, preview, rest, parentTitle } = mode;
  // A: which Item the new one goes under, ahead of what the line below says.
  const child = parentTitle === undefined ? null : <Text color="green">child of “{truncate(singleLine(parentTitle), 30)}” · </Text>;
  const draft = preview?.text === line.text ? preview.draft : undefined;
  const segments = lineSegments(line, width - LABEL.length - 1, draft ? tokenSpans(line.text, draft.tokens) : []);
  const bytes = captureBytes(line.text);
  const waiting = rest?.length ? ` · ${rest.length} more line${rest.length === 1 ? "" : "s"} to add` : "";

  let below;
  if (bytes > MAX_CAPTURE_BYTES) {
    below = (
      <Text color="red" wrap="truncate-end">
        {child}
        {bytes.toLocaleString("en")}/{MAX_CAPTURE_BYTES.toLocaleString("en")} bytes: too long to add; shorten it
        {waiting}
      </Text>
    );
  } else if (draft && line.text.trim()) {
    const warnings = [...new Set(draft.warnings.map(singleLine))];
    below = (
      <Text wrap="truncate-end">
        {child}
        {bytes >= COUNTER_FROM ? <Text color="yellow">{bytes.toLocaleString("en")}/{MAX_CAPTURE_BYTES.toLocaleString("en")} bytes · </Text> : null}
        <Text dimColor>{captureSummary(draft, today)}</Text>
        {warnings.map((w) => (
          <Text key={w} color="red">
            {" "}
            · {w}
          </Text>
        ))}
        <Text dimColor>{waiting}</Text>
      </Text>
    );
  } else {
    below = (
      <Text dimColor wrap="truncate-end">
        {child}
        {line.text.trim() ? "…" : HINT}
        {waiting}
      </Text>
    );
  }
  return (
    <Box flexDirection="column" width={width} height={2} flexShrink={0}>
      <Text wrap="truncate-end">
        <Text bold color="green">
          {LABEL}
        </Text>
        {segments.map((s, i) => (
          <Text key={i} color={s.color} inverse={s.cursor}>
            {s.text}
          </Text>
        ))}
      </Text>
      {below}
    </Box>
  );
}
