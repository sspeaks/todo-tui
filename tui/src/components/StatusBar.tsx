import { type ConnectionSnapshot, type SyncStatus, useConnection, useSyncStatus, useUndoState } from "@todo-tui/client";
import { Box, Text } from "ink";
import { useEffect, useState } from "react";
import { stateColor, stateWord } from "../controller/sync.js";
import { displayWidth, singleLine, truncate } from "../format/text.js";
import { keyFor, keyPair } from "../keys/overrides.js";
import { sequenceLabel } from "../keys/tokenize.js";
import type { UiState } from "../state/types.js";
import { tabQuery } from "../state/tabs.js";
import { useController, useOptionalView, useUi } from "./hooks.js";

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
      if (s.focus === "detail") return "DETAIL";
      return s.visual ? "VISUAL" : "NORMAL";
    case "help":
      return "HELP";
    case "report":
      return "L48";
    case "command":
      return "COMMAND";
    case "quickAdd":
      return "ADD";
    case "prompt":
    case "query":
      return "EDIT";
    case "picker":
      return "STATUS";
    case "confirm":
      return "CONFIRM";
    case "search":
      return "SEARCH";
  }
}

/** "? help" for the key the keymap in effect binds `id` to; empty when it binds none. */
function hint(s: UiState, id: string, text: string): string {
  const key = keyFor(s.keymap.bindings, id, sequenceLabel);
  return key ? `${key} ${text}` : "";
}

/** "j/k scroll" for a down and an up action. */
function pair(s: UiState, down: string, up: string, text: string): string {
  const keys = keyPair(s.keymap.bindings, down, up, sequenceLabel);
  return keys ? `${keys} ${text}` : "";
}

const joined = (...parts: string[]) => parts.filter(Boolean).join(" · ");

function hints(s: UiState): string {
  switch (s.mode.kind) {
    case "help":
      return joined(pair(s, "help_down", "help_up", "scroll"), hint(s, "help_close", "close"));
    case "report":
      return joined(hint(s, "report_copy", "copy"), pair(s, "report_down", "report_up", "scroll"), hint(s, "report_close", "close"));
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
    case "search":
      return "enter keep · esc clear";
    case "query":
      return "enter save · esc cancel";
    default:
      if (s.visual) return "move to mark · V or esc ends";
      return s.focus === "detail"
        ? joined(pair(s, "move_down", "move_up", "scroll"), hint(s, "back", "back"), hint(s, "quit", "quit"))
        : joined(hint(s, "help", "help"), hint(s, "command", "command"), hint(s, "quit", "quit"));
  }
}

/** "3 marked", or "3 marked (1 hidden)" while `/` hides some. */
function marksLabel(marks: ReadonlySet<string>, listed: readonly { id: string }[], narrowed: boolean): string {
  if (!marks.size) return "";
  const hidden = narrowed ? marks.size - listed.filter((i) => marks.has(i.id)).length : 0;
  return `${marks.size} marked${hidden ? ` (${hidden} hidden)` : ""}`;
}

/** "⇅ work-jira 2m" for each synced instance, with its colour. */
function syncLabels(all: ReadonlyMap<string, SyncStatus>, now: Date): { id: string; text: string; color: "red" | "yellow" | undefined }[] {
  return [...all.values()].map((s) => ({ id: s.instanceId, text: `⇅ ${truncate(singleLine(s.instanceId), 16)} ${stateWord(s, now)}`, color: stateColor(s.state) }));
}

/** The controller's clock, re-read every 30 s so that "2m" ages. */
function useClock(now: () => Date): Date {
  const [at, setAt] = useState(now);
  useEffect(() => {
    const t = setInterval(() => setAt(now()), 30_000);
    t.unref?.();
    return () => clearInterval(t);
  }, [now]);
  return at;
}

/**
 * Mode · connection · ⇅ sync · saving… · marks · pending prefix · the narrowing ·
 * the undo hint, and the keys for this mode on the right. The undo hint
 * and the error detail are cut first; the key hints give way when the
 * essentials would not fit beside them.
 */
export function StatusBar({ width }: { width: number }) {
  const controller = useController();
  const state = useUi((s) => s);
  const connection = useConnection();
  const undo = useUndoState().undo;
  const clock = useClock(controller.now);
  const syncs = syncLabels(useSyncStatus(), clock);
  const undoKey = keyFor(state.keymap.bindings, "undo", sequenceLabel);
  const listed = useOptionalView(state.tab === undefined ? undefined : tabQuery(state.tab, state.narrow?.shown));
  const mode = modeLabel(state);
  const conn = connectionState(connection);
  const pending = state.mode.kind === "normal" && state.mode.pending.length ? controller.whichKey(state.mode.pending) : "";
  const busy = state.busy ?? "";
  const narrowed = state.narrow?.shown;
  const rows = listed?.items ?? [];
  const marks = marksLabel(state.marks, rows, narrowed !== undefined);
  const narrow = narrowed === undefined ? "" : `/${truncate(singleLine(narrowed), 24)} · ${rows.length}`;
  const keys = ` ${hints(state)}`;
  const essential = [mode, conn, ...syncs.map((s) => s.text), busy, marks, pending, narrow].filter(Boolean).join(" · ");
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
          {syncs.map((s) => (
            <Text key={s.id} color={s.color} dimColor={s.color === undefined}>
              {" · "}
              {s.text}
            </Text>
          ))}
          {busy ? <Text color="yellow"> · {busy}</Text> : null}
          {marks ? <Text color="magenta"> · {marks}</Text> : null}
          {pending ? <Text color="cyan"> · {pending}</Text> : null}
          {narrow ? <Text color="cyan"> · {narrow}</Text> : null}
          {error ? <Text color="yellow">{error}</Text> : null}
          {undo && undoKey ? <Text dimColor> · {undoKey}: {singleLine(undo.description)}</Text> : null}
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
