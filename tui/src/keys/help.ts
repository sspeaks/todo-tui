// The help overlay's text: generated from the keymap (the bindings that run
// now, each once) and the commands, plus a cheat sheet of the capture
// syntax, the repeat rule grammar and the date grammar (design 2.3). `/`
// in it narrows it to the rows that hold every word typed (design 2.5).

import { singleLine } from "../format/text.js";
import { COMMANDS } from "./commands.js";
import { BINDINGS, type Binding, type Section, bindingModes, isAvailable, sequences } from "./keymap.js";
import { keyFor, keyPair } from "./overrides.js";
import { sequenceLabel } from "./tokenize.js";

export interface HelpLine {
  readonly text: string;
  readonly kind: "title" | "section" | "row" | "blank";
}

const SECTIONS: readonly Section[] = ["Navigation", "Views", "Items", "Marks", "Undo", "Claude", "App"];
const KEY_COLUMN = 20;

const row = (keys: string, text: string): HelpLine => ({ text: `  ${keys.padEnd(KEY_COLUMN)}${text}`, kind: "row" });

/** Lines after a section's bindings, for what no single binding says. */
const NOTES: Partial<Record<Section, readonly [string, string][]>> = {
  Marks: [
    ["x X s p e ^ @ D w W", "with marks: act on every marked Item, one undo"],
    ["M", "with marks: one meeting about all of them"],
    ["t z dd", "the same; t then takes +a -b (labels to add, remove)"],
    ["esc", "clears the marks (after the detail, V and /)"],
  ],
};

/** A binding's keys as help lists them: "j ↓", "gg home". */
export const bindingKeys = (b: Binding): string => b.helpKeys ?? sequences(b).map(sequenceLabel).join(" ");

/** The normal-mode bindings that run now, in table order, one per help row (a helpKeys group counts once). */
export function helpBindings(bindings: readonly Binding[] = BINDINGS): Binding[][] {
  const rows = new Map<string, Binding[]>();
  for (const b of bindings) {
    if (!b.keys.length) continue;
    if (!bindingModes(b).includes("normal") || !isAvailable(b)) continue;
    const key = b.helpKeys ?? b.id;
    rows.set(key, [...(rows.get(key) ?? []), b]);
  }
  return [...rows.values()];
}

const CAPTURE: readonly [string, string][] = [
  ["+1 … +5", "priority, +1 highest"],
  ["%low %med %high", "energy: how much focus it takes"],
  ["^<date>", "due date (dates below)"],
  ["@<rule>", "repeats (rules below)"],
  ["#label", "label"],
  ["~dana ~\"the vendor\"", "waiting on"],
  [">next >someday", "status (default inbox)"],
  ["\\#1", "a sigil kept as text"],
];

const DATES: readonly [string, string][] = [
  ["today tod", "today"],
  ["tomorrow tom", "tomorrow"],
  ["mon … sun", "the next one after today"],
  ["+3d +2w +1m", "days, weeks, months from today"],
  ["eow eom", "this Friday, the month's last day"],
  ["2026-11-02", "that date"],
  ["nov2", "the next Nov 2"],
];

const RULES: readonly [string, string][] = [
  ["daily weekdays", "every day, Mon–Fri"],
  ["weekly monthly", "on the ^date's weekday or day, else today's"],
  ["mon,thu", "on those weekdays"],
  ["15th", "that day of every month (the last, if shorter)"],
  ["3d 2w", "days, weeks after done"],
];

interface HelpSection {
  readonly name: string;
  readonly rows: readonly HelpLine[];
}

/** The help's sections, each with its rows: the keymap's, the commands, the cheat sheets. */
function helpSections(bindings: readonly Binding[]): HelpSection[] {
  const sections: HelpSection[] = [];
  const rows = helpBindings(bindings);
  for (const name of SECTIONS) {
    const inSection = rows.filter(([first]) => first?.section === name);
    if (!inSection.length) continue;
    const lines: HelpLine[] = [];
    for (const group of inSection) if (group[0]) lines.push(row(bindingKeys(group[0]), group[0].help));
    for (const [k, t] of NOTES[name] ?? []) lines.push(row(k, t));
    sections.push({ name, rows: lines });
  }
  const commands = COMMANDS.filter((c) => c.runs).map((c) =>
    row([c.name, ...(c.aliases ?? [])].map((n) => `:${n}`).join(" ") + (c.args ? ` ${c.args}` : ""), c.help),
  );
  sections.push({ name: "Commands", rows: commands });
  const sheet = (name: string, pairs: readonly [string, string][]) => sections.push({ name, rows: pairs.map(([k, t]) => row(k, t)) });
  sheet("Capture syntax", CAPTURE);
  sheet("Repeat rules", RULES);
  sheet("Dates", DATES);
  return sections;
}

/** The words a help search looks for, lower-cased: a row is shown when it holds every one, in any case. */
export const searchWords = (query: string): string[] => query.toLowerCase().split(/\s+/u).filter(Boolean);

/** Whether a row, or its section's name, holds every word (a word never spans the two). */
const matches = (section: string, text: string, words: readonly string[]): boolean => {
  const haystack = `${section} ${text}`.toLowerCase();
  return words.every((w) => haystack.includes(w));
};

/**
 * The help for `bindings`, the keymap in effect with its [keymap]
 * overrides. With `query`, only the rows that match it, each under its
 * section, and the title says what it is narrowed to. While the search
 * line is open (`typing`) its keys are text, so the title names none.
 */
export function helpLines(bindings: readonly Binding[] = BINDINGS, query = "", typing = false): HelpLine[] {
  const hint = (k: string | undefined, text: string) => (k ? ` · ${k} ${text}` : "");
  const scroll = hint(keyPair(bindings, "help_down", "help_up", sequenceLabel), "scroll");
  const search = hint(keyFor(bindings, "help_search", sequenceLabel), "search");
  const close = hint(keyFor(bindings, "help_close", sequenceLabel), "close");
  const words = searchWords(query);
  const sections = helpSections(bindings)
    .map((s) => (words.length ? { ...s, rows: s.rows.filter((r) => matches(s.name, r.text, words)) } : s))
    .filter((s) => s.rows.length);
  const body = sections.flatMap((s): HelpLine[] => [{ text: "", kind: "blank" }, { text: s.name, kind: "section" }, ...s.rows]);
  const typed = `Help /${singleLine(query.trim())}`;
  if (!words.length) return [{ text: typing ? typed : `Help${scroll}${search}${close}`, kind: "title" }, ...body];
  const count = sections.reduce((n, s) => n + s.rows.length, 0);
  const found = `${typed} · ${count ? `${count} ${count === 1 ? "row" : "rows"}` : "no row matches"}`;
  return [{ text: typing ? found : `${found}${count ? scroll : ""}${close}`, kind: "title" }, ...body];
}

const escapeRegExp = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");

/**
 * `text` cut into runs, those inside an occurrence of one of `words` (in
 * any case) marked: what a help search found, lit up. Occurrences may
 * overlap ("do" and "own" in "down"); every one is lit.
 */
export function matchRuns(text: string, words: readonly string[]): { readonly text: string; readonly match: boolean }[] {
  if (!words.length) return [{ text, match: false }];
  const lit = new Array<boolean>(text.length).fill(false);
  for (const word of words) {
    const re = new RegExp(escapeRegExp(word), "giu");
    for (let m = re.exec(text); m; m = re.exec(text)) {
      lit.fill(true, m.index, m.index + m[0].length);
      // Overlapping occurrences of the same word too ("aa" in "aaa").
      re.lastIndex = m.index + 1;
    }
  }
  const runs: { text: string; match: boolean }[] = [];
  let at = 0;
  for (let i = 1; i <= text.length; i++) {
    if (i < text.length && lit[i] === lit[at]) continue;
    runs.push({ text: text.slice(at, i), match: lit[at]! });
    at = i;
  }
  return runs;
}
