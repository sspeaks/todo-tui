import { type ConnectionSnapshot, useConnection, useUndoState } from "@todo-tui/client";
import { Box, Text } from "ink";
import { displayWidth, singleLine } from "../format/text.js";
import type { UiState } from "../state/types.js";
import { useController, useUi } from "./hooks.js";

const describe = (err: unknown) => (err instanceof Error ? err.message : String(err));

/** "● live", or "◌ reconnecting (3)". */
function connectionState({ state, attempt }: ConnectionSnapshot): string {
  if (state === "live") return "● live";
  const tries = state === "reconnecting" && attempt > 1 ? ` (${attempt})` : "";
  return `◌ ${state}${tries}`;
}

/** Why it is not live, when known. */
const connectionError = ({ state, error }: ConnectionSnapshot): string =>
  state !== "live" && error !== undefined ? ` · ${singleLine(describe(error))}` : "";

function modeLabel(s: UiState): string {
  switch (s.mode.kind) {
    case "normal":
      return s.focus === "detail" ? "DETAIL" : "NORMAL";
    case "help":
      return "HELP";
    case "command":
      return "COMMAND";
    case "quickAdd":
      return "ADD";
    case "prompt":
      return "EDIT";
    case "picker":
      return "STATUS";
    case "confirm":
      return "CONFIRM";
    default:
      return s.mode.kind.toUpperCase();
  }
}

function hints(s: UiState): string {
  switch (s.mode.kind) {
    case "help":
      return "j/k scroll · esc close";
    case "command":
      return "enter run · tab complete · esc cancel";
    case "quickAdd":
      return "enter add · enter on empty closes · esc discard";
    case "prompt":
      return s.mode.prompt === "waiting" ? "enter save · tab complete · esc cancel" : "enter save · esc cancel";
    case "picker":
      return "letter or j/k enter · esc cancel";
    case "confirm":
      return "y yes · any other key no";
    default:
      return s.focus === "detail" ? "j/k scroll · esc back · q quit" : "? help · : command · q quit";
  }
}

/**
 * Mode · connection · saving… · pending prefix · the undo hint, and the keys for this
 * mode on the right. The undo hint and the error detail are cut first; the
 * key hints give way when the mode, the connection and a pending prefix
 * would not fit beside them.
 */
export function StatusBar({ width }: { width: number }) {
  const controller = useController();
  const state = useUi((s) => s);
  const connection = useConnection();
  const undo = useUndoState().undo;
  const mode = modeLabel(state);
  const conn = connectionState(connection);
  const pending = state.mode.kind === "normal" && state.mode.pending.length ? controller.whichKey(state.mode.pending) : "";
  const busy = state.busy ?? "";
  const keys = ` ${hints(state)}`;
  const essential = `${mode} · ${conn}${busy ? ` · ${busy}` : ""}${pending ? ` · ${pending}` : ""}`;
  const showKeys = displayWidth(essential) + displayWidth(keys) <= width;
  const live = connection.state === "live";
  const error = connectionError(connection);
  return (
    <Box width={width} height={1}>
      <Box flexGrow={1} flexShrink={1}>
        <Text wrap="truncate-end">
          <Text bold>{mode}</Text>
          <Text dimColor> · </Text>
          <Text color={live ? "green" : "yellow"}>{conn}</Text>
          {busy ? <Text color="yellow"> · {busy}</Text> : null}
          {pending ? <Text color="cyan"> · {pending}</Text> : null}
          {error ? <Text color="yellow">{error}</Text> : null}
          {undo ? <Text dimColor> · u: {singleLine(undo.description)}</Text> : null}
        </Text>
      </Box>
      {showKeys ? (
        <Box flexShrink={0}>
          <Text dimColor>{keys}</Text>
        </Box>
      ) : null}
    </Box>
  );
}
