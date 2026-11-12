// The `:` command line (Decision 5): the commands M4 runs, prefix
// completion over them, and a pointer to the milestone that brings each
// planned one.

export type CommandName = "quit" | "help" | "refresh" | "search" | "config" | "view" | "export" | "l48" | "meet" | "sync" | "suggestions" | "apply" | "refine" | "promote" | "link";

interface CommandSpec {
  readonly name: string;
  readonly aliases?: readonly string[];
  readonly help: string;
  /** What it runs; unset while it is still coming in M4. */
  readonly runs?: CommandName;
  /** Arguments it takes, for help: "<query>". */
  readonly args?: string;
}

export const COMMANDS: readonly CommandSpec[] = [
  { name: "quit", aliases: ["q"], help: "Quit", runs: "quit" },
  { name: "help", aliases: ["h"], help: "Keys, capture syntax and dates", runs: "help" },
  { name: "refresh", help: "Re-read every view and the counts", runs: "refresh" },
  { name: "search", aliases: ["s"], help: "Search everything, done, archived and deferred too", runs: "search", args: "<query>" },
  { name: "config", help: "The config file and what is wrong with it", runs: "config" },
  { name: "view", help: "Views: new <name> [query], edit, rename, delete, move", runs: "view", args: "<sub> …" },
  { name: "l48", help: "The L48 / N48 report for your scrum; y copies it", runs: "l48", args: "[since]" },
  { name: "meet", help: "Schedule a Teams meeting about the Item, or the marked ones", runs: "meet" },
  { name: "sync", help: "Sync with Jira now; :sync status says how it stands, :sync outbox what waits", runs: "sync", args: "[status | outbox | instance]" },
  { name: "suggestions", help: "The Suggestions pane: what Jira won over, and what it refused", runs: "suggestions" },
  { name: "refine", help: "Ask Claude to refine the Item, or the marked ones (proposals land in S)", runs: "refine" },
  { name: "promote", help: "Make the Item a Jira issue, in the project given or promote_default's", runs: "promote", args: "[PROJECT]" },
  { name: "link", help: "Link the Item to a Jira issue by key", runs: "link", args: "KEY" },
  { name: "w", help: "In the Suggestions pane: apply what is ticked", runs: "apply" },
  { name: "export", help: "Write every record to a file (~/todo-export-….jsonl)", runs: "export", args: "[path]" },
];

/** Commands from the design that a later milestone brings, and where they arrive. */
const PLANNED: Readonly<Record<string, string>> = {
  review: "with the weekly review in P4",
  standup: "with the standup in P4",
};

export type CommandResult =
  | { kind: "run"; command: CommandName; args: string }
  | { kind: "message"; level: "info" | "error"; text: string }
  | { kind: "none" };

const names = (c: CommandSpec) => [c.name, ...(c.aliases ?? [])];

/** The commands whose name (or alias) starts with `prefix`. */
const matching = (prefix: string) => COMMANDS.filter((c) => names(c).some((n) => n.startsWith(prefix)));

/** What a submitted command line does: a command by name, alias or unique prefix. */
export function resolveCommand(line: string): CommandResult {
  const text = line.trim();
  if (text === "") return { kind: "none" };
  const [word = "", ...rest] = text.split(/\s+/);
  const args = rest.join(" ");
  const exact = COMMANDS.find((c) => names(c).includes(word));
  const planned = PLANNED[word];
  if (!exact && planned !== undefined) return { kind: "message", level: "info", text: `:${word} arrives ${planned}` };
  const found = exact ? [exact] : matching(word);
  const [command] = found;
  if (!command) return { kind: "message", level: "error", text: `Not a command: ${word}` };
  if (found.length > 1) return { kind: "message", level: "error", text: `Ambiguous: ${found.map((c) => `:${c.name}`).join(" ")}` };
  if (!command.runs) return { kind: "message", level: "info", text: `:${command.name} is coming soon` };
  return { kind: "run", command: command.runs, args };
}

/**
 * Tab completion: the first word extended to the longest prefix all
 * matching commands share; after `view`, its subcommand, then a view name
 * from `views` (quoted when it has a space).
 */
export function completeCommand(line: string, views: readonly string[] = []): string {
  const sub = /^(\s*view\s+)(\S*)$/.exec(line);
  if (sub) return completeFrom(line, sub[1] ?? "", sub[2] ?? "", VIEW_SUBS, true);
  const named = /^(\s*view\s+(?:edit|rename|delete|move)\s+)("?[^"\s]*)$/.exec(line);
  if (named) return completeFrom(line, named[1] ?? "", (named[2] ?? "").replace(/^"/, ""), views, false, true);
  if (/\s/.test(line.trimStart())) return line;
  const word = line.trim();
  const found = matching(word).map((c) => c.name);
  const [first] = found;
  if (first === undefined) return line;
  if (found.length === 1) return COMMANDS.find((c) => c.name === first)?.args ? `${first} ` : first;
  let common = first;
  for (const n of found) while (!n.startsWith(common)) common = common.slice(0, -1);
  return common.length > word.length ? common : line;
}

const VIEW_SUBS = ["new", "edit", "rename", "delete", "move"];

/**
 * `head` then `word` completed from `options` (ignoring case, for names): a
 * unique one whole (quoted when it has a space; with a space after, for a
 * subcommand), else as far as they all agree.
 */
function completeFrom(line: string, head: string, word: string, options: readonly string[], space: boolean, ignoreCase = false): string {
  const norm = (s: string) => (ignoreCase ? s.toLowerCase() : s);
  const found = options.filter((o) => norm(o).startsWith(norm(word)));
  const [first] = found;
  if (first === undefined) return line;
  const quote = (s: string) => (/[\s"\\]/.test(s) ? `"${s.replace(/["\\]/g, "\\$&")}"` : s);
  if (found.length === 1) return `${head}${quote(first)}${space ? " " : ""}`;
  let common = first;
  for (const o of found) while (!norm(o).startsWith(norm(common))) common = common.slice(0, -1);
  return common.length > word.length && !/\s/.test(common) ? `${head}${common}` : line;
}
