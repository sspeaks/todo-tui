// `/` in the help (design 2.5): narrows it to the rows that hold every
// word typed, live, each under its section with the words lit up. enter
// keeps it, so that j/k scroll what is left, and `/` edits it again; esc
// while typing shows all of the help again, where it was. Closing the help
// drops it.

import { searchWords } from "../keys/help.js";
import { editLine, insert, type LineState, lineOf } from "../keys/line-editor.js";
import { isChar, type KeyToken } from "../keys/tokenize.js";
import type { Mode } from "../state/types.js";

export type HelpMode = Extract<Mode, { kind: "help" }>;

/** All of the help, at the scroll from before `/`. */
const all = (mode: HelpMode): HelpMode => ({ kind: "help", scroll: mode.saved ?? mode.scroll });

/** `/`: the search line, holding the text the help is narrowed to. */
export const openHelpSearch = (mode: HelpMode): HelpMode => ({
  kind: "help",
  scroll: mode.scroll,
  query: mode.query ?? "",
  line: lineOf(mode.query ?? ""),
  saved: mode.saved ?? mode.scroll,
});

/**
 * The help narrowed to `line`'s text as it is typed, from its top; blank
 * text shows all of it, where it was. An edit that leaves the words as
 * they were (a cursor move, a space) keeps the scroll.
 */
export function typedHelp(mode: HelpMode, line: LineState): HelpMode {
  const saved = mode.saved ?? mode.scroll;
  const words = searchWords(line.text);
  const same = words.join(" ") === searchWords(mode.query ?? "").join(" ");
  return { kind: "help", scroll: same ? mode.scroll : words.length ? 0 : saved, query: line.text, line, saved };
}

/** enter: keeps the narrowing (text that narrows nothing shows all of the help). */
function keep(mode: HelpMode): HelpMode {
  if (!searchWords(mode.query ?? "").length) return all(mode);
  const { line: _, ...kept } = mode;
  return kept;
}

/** A key on the help's search line: the next mode, or undefined when the key does nothing. `chars` takes a burst of typed characters. */
export function helpSearchKey(mode: HelpMode, line: LineState, key: KeyToken, chars: () => string): HelpMode | undefined {
  switch (key) {
    case "esc":
    case "ctrl+c":
      return all(mode);
    case "enter":
      return keep(mode);
    case "tab":
      return undefined;
    case "backspace":
      // As in vim, and as `/` in the list, backspace on an empty search leaves it.
      if (line.text === "") return all(mode);
  }
  const next = isChar(key) ? insert(line, chars()) : editLine(line, key);
  return next && typedHelp(mode, next);
}
