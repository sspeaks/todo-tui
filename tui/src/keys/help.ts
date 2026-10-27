// The help overlay's text: generated from the keymap (the bindings that run
// now, each once) and the commands, plus a cheat sheet of the capture
// syntax and the date grammar (design 2.3).

import { COMMANDS } from "./commands.js";
import { BINDINGS, type Binding, type Section, bindingModes, isAvailable, sequences } from "./keymap.js";
import { sequenceLabel } from "./tokenize.js";

export interface HelpLine {
  readonly text: string;
  readonly kind: "title" | "section" | "row" | "blank";
}

const SECTIONS: readonly Section[] = ["Navigation", "Views", "Items", "Marks", "Undo", "App"];
const KEY_COLUMN = 20;

const row = (keys: string, text: string): HelpLine => ({ text: `  ${keys.padEnd(KEY_COLUMN)}${text}`, kind: "row" });

/** Lines after a section's bindings, for what no single binding says. */
const NOTES: Partial<Record<Section, readonly [string, string][]>> = {
  Marks: [
    ["x X s p ^ D w W", "with marks: act on every marked Item, one undo"],
    ["t z dd", "the same; t then takes +a -b (labels to add, remove)"],
    ["esc", "clears the marks (after the detail, V and /)"],
  ],
};
const section = (text: string): HelpLine[] => [{ text: "", kind: "blank" }, { text, kind: "section" }];

/** A binding's keys as help lists them: "j ↓", "gg home". */
export const bindingKeys = (b: Binding): string => b.helpKeys ?? sequences(b).map(sequenceLabel).join(" ");

/** The normal-mode bindings that run now, in table order, one per help row (a helpKeys group counts once). */
export function helpBindings(): Binding[][] {
  const rows = new Map<string, Binding[]>();
  for (const b of BINDINGS) {
    if (!bindingModes(b).includes("normal") || !isAvailable(b)) continue;
    const key = b.helpKeys ?? b.id;
    rows.set(key, [...(rows.get(key) ?? []), b]);
  }
  return [...rows.values()];
}

const CAPTURE: readonly [string, string][] = [
  ["+1 … +5", "priority, +1 highest"],
  ["^<date>", "due date (dates below)"],
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

export function helpLines(): HelpLine[] {
  const lines: HelpLine[] = [{ text: "Help · j/k scroll · esc close", kind: "title" }];
  const rows = helpBindings();
  for (const name of SECTIONS) {
    const inSection = rows.filter(([first]) => first?.section === name);
    if (!inSection.length) continue;
    lines.push(...section(name));
    for (const group of inSection) if (group[0]) lines.push(row(bindingKeys(group[0]), group[0].help));
    for (const [k, t] of NOTES[name] ?? []) lines.push(row(k, t));
  }
  lines.push(...section("Commands"));
  for (const c of COMMANDS) {
    if (!c.runs) continue;
    lines.push(row([c.name, ...(c.aliases ?? [])].map((n) => `:${n}`).join(" ") + (c.args ? ` ${c.args}` : ""), c.help));
  }
  lines.push(...section("Capture syntax"));
  for (const [k, t] of CAPTURE) lines.push(row(k, t));
  lines.push(...section("Dates"));
  for (const [k, t] of DATES) lines.push(row(k, t));
  return lines;
}
