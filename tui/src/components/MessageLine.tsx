import { Box, Text } from "ink";
import { STATUS_CHOICES, sharedStatus } from "../controller/actions.js";
import { PROMPT_LABELS } from "../controller/prompts.js";
import { displayWidth, sanitize, singleLine, truncate } from "../format/text.js";
import { type LineState, lineWindow, visibleLine } from "../keys/line-editor.js";
import type { Mode } from "../state/types.js";
import { useController, useUi } from "./hooks.js";

/** A line being edited after its label, the cursor's cell in inverse; control characters (a title's ESC) left out. */
function Editor({ label, line, width }: { label: string; line: LineState; width: number }) {
  const { before, under, after } = visibleLine(line, width - label.length - 1);
  return (
    <Text wrap="truncate-end">
      <Text bold>{label}</Text>
      {sanitize(before)}
      <Text inverse>{sanitize(under) || " "}</Text>
      {sanitize(after)}
    </Text>
  );
}

/** A query line after its label, the span of its first error in red. */
function QueryEditor({ label, line, width, span }: { label: string; line: LineState; width: number; span?: { start: number; end: number } }) {
  const cps = [...line.text];
  const { start, end } = lineWindow(line, width - displayWidth(label) - 1);
  const runs: { text: string; red: boolean; cursor: boolean }[] = [];
  for (let i = start; i < Math.max(end, line.cursor + 1); i++) {
    const cursor = i === line.cursor;
    const text = sanitize(cps[i] ?? " ") || (cursor ? " " : "");
    if (!text) continue;
    const red = span !== undefined && i >= span.start && i < span.end;
    const last = runs.at(-1);
    if (last && !cursor && !last.cursor && last.red === red) last.text += text;
    else runs.push({ text, red, cursor });
  }
  return (
    <Text wrap="truncate-end">
      <Text bold>{label}</Text>
      {runs.map((r, i) => (
        <Text key={i} color={r.red ? "red" : undefined} inverse={r.cursor}>
          {r.text}
        </Text>
      ))}
    </Text>
  );
}

/** " (3 marked)" after a prompt's label when it acts on the marks. */
const marked = (target: { bulk: boolean; ids: readonly string[] }) => (target.bulk ? ` (${target.ids.length} marked)` : "");

/** The status picker: a letter per status, the one j/k is on in inverse, the current status ticked (with marks, the one they share). */
function Picker({ mode }: { mode: Extract<Mode, { kind: "picker" }> }) {
  const { store } = useController();
  const current = sharedStatus(mode.target, store.getItem);
  return (
    <Text wrap="truncate-end">
      <Text bold>Status{marked(mode.target)}: </Text>
      {STATUS_CHOICES.map((c, i) => (
        <Text key={c.key}>
          {i ? <Text dimColor> · </Text> : null}
          <Text inverse={i === mode.index}>
            <Text bold>{c.key}</Text> {c.name}
            {c.status === current ? " ✓" : ""}
          </Text>
        </Text>
      ))}
    </Text>
  );
}

/** The bottom line: the command line, a prompt, the status picker or the dd confirm while open, else the toast. */
export function MessageLine({ width }: { width: number }) {
  const mode = useUi((s) => s.mode);
  const toast = useUi((s) => s.toast);
  let content;
  if (mode.kind === "command") {
    content = <Editor label=":" line={mode.line} width={width} />;
  } else if (mode.kind === "search") {
    content = <Editor label="/" line={mode.line} width={width} />;
  } else if (mode.kind === "prompt") {
    content = <Editor label={`${PROMPT_LABELS[mode.prompt]}${marked(mode.target)}: `} line={mode.line} width={width} />;
  } else if (mode.kind === "query") {
    const span = mode.preview?.for === mode.line.text ? mode.preview.span : undefined;
    content = <QueryEditor label={`View “${truncate(singleLine(mode.name), 24)}”: `} line={mode.line} width={width} {...(span && { span })} />;
  } else if (mode.kind === "picker") {
    content = <Picker mode={mode} />;
  } else if (mode.kind === "confirm") {
    content = (
      <Text wrap="truncate-end" color="yellow" bold>
        {singleLine(mode.text)}
      </Text>
    );
  } else if (toast) {
    content = (
      <Text wrap={toast.keepEnd ? "truncate-middle" : "truncate-end"} color={toast.level === "error" ? "red" : undefined}>
        {singleLine(toast.text)}
      </Text>
    );
  } else {
    content = <Text> </Text>;
  }
  return (
    <Box width={width} height={1}>
      {content}
    </Box>
  );
}

/** What a prompt's line says before enter, when there is no preview yet. */
const PROMPT_HINTS: Readonly<Record<Extract<Mode, { kind: "prompt" }>["prompt"], string>> = {
  title: "enter saves · esc cancels",
  labels: "#a #b: the whole set · enter saves · esc cancels",
  due: "a date: fri, +3d, eow, nov2, 2026-11-02 · empty clears",
  defer: "a date: fri, +3d, eow, nov2, 2026-11-02 · empty clears",
  waiting: "<person> [^date] · tab completes the person · empty clears",
  repeat: "a rule: daily, weekdays, mon,thu, 15th, 3d, 2w · empty stops it",
  meeting: "the subject · enter opens Teams, prefilled · esc cancels",
};

/** The hints of a prompt acting on marks, where they differ. */
const BULK_HINTS: Partial<Record<Extract<Mode, { kind: "prompt" }>["prompt"], string>> = {
  labels: "+a adds · -b removes, on each marked Item · enter saves · esc cancels",
  waiting: "<person> [^date] for each marked Item · tab completes the person · empty clears",
  repeat: "a rule for each marked Item: daily, weekdays, mon,thu, 15th, 3d, 2w · empty stops it",
};

/**
 * Above the status bar while a prompt is open: why enter did not save, a
 * conflict (the server's value, which enter overwrites), the ParseDate or
 * ParseRecurrence preview, or a hint. A toast while the prompt is open
 * shows here too.
 */
export function PromptPreviewLine({ width }: { width: number }) {
  const mode = useUi((s) => (s.mode.kind === "prompt" ? s.mode : undefined));
  const toast = useUi((s) => s.toast);
  if (!mode) return null;
  let content;
  if (toast?.level === "error" || mode.error !== undefined) {
    content = (
      <Text color="red" wrap="truncate-end">
        {singleLine(mode.error ?? toast?.text ?? "")}
      </Text>
    );
  } else if (mode.conflict !== undefined) {
    content = (
      <Text color="yellow" wrap="truncate-end">
        Changed elsewhere to “{truncate(singleLine(mode.conflict), Math.max(10, width - 45))}” — enter overwrites · esc discards
      </Text>
    );
  } else if (mode.preview && mode.preview.for === mode.line.text) {
    content = (
      <Text color={mode.preview.warn ? "red" : "cyan"} wrap="truncate-end">
        {singleLine(mode.preview.text)}
      </Text>
    );
  } else if (toast) {
    content = <Text wrap="truncate-end">{singleLine(toast.text)}</Text>;
  } else {
    content = (
      <Text dimColor wrap="truncate-end">
        {(mode.target.bulk && BULK_HINTS[mode.prompt]) || PROMPT_HINTS[mode.prompt]}
      </Text>
    );
  }
  return (
    <Box width={width} height={1} flexShrink={0}>
      {content}
    </Box>
  );
}

const QUERY_HINT = "#label is:next priority>=high due_date<=today+3d any:( … ) sort:-priority count:off · enter saves";

/** Above the status bar while a :view prompt is open: why enter did not save, a conflict, the preview, or a hint. */
export function QueryPreviewLine({ width }: { width: number }) {
  const mode = useUi((s) => (s.mode.kind === "query" ? s.mode : undefined));
  const toast = useUi((s) => s.toast);
  if (!mode) return null;
  let content;
  if (toast?.level === "error" || mode.error !== undefined) {
    content = (
      <Text color="red" wrap="truncate-end">
        {singleLine(mode.error ?? toast?.text ?? "")}
      </Text>
    );
  } else if (mode.conflict !== undefined) {
    content = (
      <Text color="yellow" wrap="truncate-end">
        Changed elsewhere to “{truncate(singleLine(mode.conflict), Math.max(10, width - 45))}” — enter overwrites · esc discards
      </Text>
    );
  } else if (mode.preview && mode.preview.for === mode.line.text) {
    content = (
      <Text color={mode.preview.warn ? "red" : "cyan"} wrap="truncate-end">
        {singleLine(mode.preview.text)}
      </Text>
    );
  } else {
    content = (
      <Text dimColor wrap="truncate-end">
        {QUERY_HINT}
      </Text>
    );
  }
  return (
    <Box width={width} height={1} flexShrink={0}>
      {content}
    </Box>
  );
}
