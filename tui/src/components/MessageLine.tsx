import { Box, Text } from "ink";
import { singleLine } from "../format/text.js";
import { visibleLine } from "../keys/line-editor.js";
import { useUi } from "./hooks.js";

/** The bottom line: the command line while it is open, else the toast. */
export function MessageLine({ width }: { width: number }) {
  const mode = useUi((s) => s.mode);
  const toast = useUi((s) => s.toast);
  let content;
  if (mode.kind === "command") {
    const { before, under, after } = visibleLine(mode.line, width - 1);
    content = (
      <Text wrap="truncate-end">
        :{before}
        <Text inverse>{under}</Text>
        {after}
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
