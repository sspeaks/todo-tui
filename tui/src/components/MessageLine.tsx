import { Box, Text } from "ink";
import { STATUS_CHOICES } from "../controller/actions.js";
import { PROMPT_LABELS } from "../controller/prompts.js";
import { sanitize, singleLine, truncate } from "../format/text.js";
import { type LineState, visibleLine } from "../keys/line-editor.js";
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

/** The status picker: a letter per status, the one j/k is on in inverse, the Item's current status ticked. */
function Picker({ mode }: { mode: Extract<Mode, { kind: "picker" }> }) {
  const { store } = useController();
  const current = store.getItem(mode.target.ids[0] ?? "")?.status;
  return (
    <Text wrap="truncate-end">
      <Text bold>Status: </Text>
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
  } else if (mode.kind === "prompt") {
    content = <Editor label={`${PROMPT_LABELS[mode.prompt]}: `} line={mode.line} width={width} />;
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
      <Text wrap="truncate-end" color={toast.level === "error" ? "red" : undefined}>
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
};

/**
 * Above the status bar while a prompt is open: why enter did not save, a
 * conflict (the server's value, which enter overwrites), the ParseDate
 * preview, or a hint. A toast while the prompt is open shows here too.
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
        {PROMPT_HINTS[mode.prompt]}
      </Text>
    );
  }
  return (
    <Box width={width} height={1} flexShrink={0}>
      {content}
    </Box>
  );
}
