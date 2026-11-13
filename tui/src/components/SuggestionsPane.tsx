import { useItem, usePeople, useSuggestions } from "@todo-tui/client";
import { Box, Text } from "ink";
import { type PaneRow, cursorIndex, paneRows } from "../controller/suggestions.js";
import { closedWord, fromClaude, headingText, patchText } from "../format/suggestion.js";
import { singleLine, wrap } from "../format/text.js";
import type { Mode } from "../state/types.js";
import { useToday, useUi } from "./hooks.js";

/** The pane's mode: its own, or the one a command line or question opened from it returns to. */
export function paneMode(mode: Mode): Extract<Mode, { kind: "suggestions" }> | undefined {
  if (mode.kind === "suggestions") return mode;
  if (mode.kind === "command") return mode.back;
  if (mode.kind === "confirm" && mode.action === "not_mine" && mode.back?.kind === "suggestions") return mode.back;
  return undefined;
}

/**
 * One row: a heading, or a change: a sync one "[ ] title  theirs (Jira) →
 * mine (yours)", Claude's "[x] title  as it is (now) → proposed (Claude)".
 */
function Row({ row, selected, ticked, today, width }: { row: PaneRow; selected: boolean; ticked: boolean; today: number; width: number }) {
  // An Item no loaded view holds is read once: the heading names it.
  const item = useItem(row.suggestion.itemId, { fetch: true });
  const people = usePeople().people;
  const mark = selected ? "› " : "  ";
  if (row.kind === "heading") {
    return (
      <Text wrap="truncate-end" bold inverse={selected}>
        {mark}▾ {headingText(row.suggestion, item)}
      </Text>
    );
  }
  const { field, from, to } = patchText(row.suggestion, row.ids, item, today, people);
  const closed = row.closed ? closedWord(row.suggestion.patches.find((p) => row.ids.includes(p.id))!) : undefined;
  const label = field.padEnd(9);
  // Whose each side is: Jira's and yours for sync, the Item's and Claude's for a refine.
  const [was, will] = fromClaude(row.suggestion) ? ["(now)", "(Claude)"] : ["(Jira)", "(yours)"];
  return (
    <Box width={width}>
      <Text wrap="truncate-end" inverse={selected} strikethrough={row.closed} dimColor={row.closed}>
        {mark}
        {"   "}
        {row.closed ? "[-]" : ticked ? "[x]" : "[ ]"} {label} {from} <Text dimColor>{was}</Text> → {to} <Text dimColor>{will}</Text>
        {closed ? <Text dimColor> · {closed}</Text> : null}
      </Text>
    </Box>
  );
}

/**
 * The Suggestions pane (design 2.6): each Suggestion's heading with what
 * it is about and why, a push_failed's error or Claude's rationale (wrapped)
 * under it, then its changes as checkboxes. Windowed to the body around
 * the cursor.
 */
export function SuggestionsPane({ width, height }: { width: number; height: number }) {
  const mode = useUi((s) => paneMode(s.mode));
  const snap = useSuggestions();
  const today = useToday();
  if (!mode) return null;
  const rows = paneRows(snap.suggestions);
  const total = new Set(snap.suggestions.map((s) => s.itemId)).size;
  const changes = rows.filter((r) => r.kind === "patch" && !r.closed).length;
  const title = `✦ Suggestions (${total} item${total === 1 ? "" : "s"}, ${changes} change${changes === 1 ? "" : "s"})`;
  if (snap.status === "loading") {
    return (
      <Box height={height} paddingLeft={1}>
        <Text dimColor>Loading the Suggestions…</Text>
      </Box>
    );
  }
  if (snap.status === "error" && !rows.length) {
    return (
      <Box flexDirection="column" height={height} paddingLeft={1}>
        <Text bold>✦ Suggestions</Text>
        <Text color="red" wrap="truncate-end">
          Could not read the Suggestions: {singleLine(snap.error instanceof Error ? snap.error.message : String(snap.error))}
        </Text>
      </Box>
    );
  }
  if (!rows.length) {
    return (
      <Box flexDirection="column" height={height} paddingLeft={1}>
        <Text bold>✦ Suggestions</Text>
        <Text dimColor>Nothing to review: no change waits on you, from Jira or from Claude.</Text>
      </Box>
    );
  }
  // Each push_failed heading takes one more line, for its error; Claude's
  // rationale is wrapped onto lines of its own under its heading.
  const indent = "       ";
  const lines: { row: PaneRow; index: number; error?: string; note?: string }[] = [];
  rows.forEach((row, index) => {
    lines.push({ row, index });
    if (row.kind !== "heading") return;
    if (row.suggestion.error) lines.push({ row, index, error: row.suggestion.error });
    if (fromClaude(row.suggestion) && row.suggestion.rationale.trim()) {
      for (const note of wrap(singleLine(row.suggestion.rationale), Math.max(10, width - indent.length - 1))) lines.push({ row, index, note });
    }
  });
  const cursor = cursorIndex(rows, mode);
  const body = Math.max(1, height - 1);
  const at = lines.findIndex((l) => l.index === cursor && l.error === undefined && l.note === undefined);
  const top = Math.max(0, Math.min(at - Math.floor(body / 2), lines.length - body));
  return (
    <Box flexDirection="column" width={width} height={height} overflow="hidden">
      <Text bold wrap="truncate-end">
        {title}
      </Text>
      {lines.slice(top, top + body).map((l, i) =>
        l.note !== undefined ? (
          <Text key={`n${top + i}`} wrap="truncate-end" dimColor>
            {indent}
            {l.note}
          </Text>
        ) : l.error !== undefined ? (
          <Text key={`e${top + i}`} wrap="truncate-end" color="red">
            {"       "}Jira: {singleLine(l.error)}
          </Text>
        ) : (
          <Row
            key={`r${top + i}`}
            row={l.row}
            selected={l.index === cursor}
            ticked={l.row.kind === "patch" && l.row.ids.every((id) => mode.ticked.has(id))}
            today={today}
            width={width}
          />
        ),
      )}
    </Box>
  );
}
