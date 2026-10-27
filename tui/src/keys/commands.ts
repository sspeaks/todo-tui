// The `:` command line (Decision 5): the commands M4 runs, prefix
// completion over them, and a pointer to the milestone that brings each
// planned one.

export type CommandName = "quit" | "help" | "refresh";

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
  { name: "search", help: "Search everything", args: "<query>" },
];

/** Commands from the design that a later milestone brings, and where they arrive. */
const PLANNED: Readonly<Record<string, string>> = {
  view: "with view editing in M6",
  export: "in M6",
  sync: "with Jira sync in P2",
  promote: "with Jira sync in P2",
  link: "with Jira sync in P2",
  w: "with the Suggestions pane in P2",
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

/** Tab completion: the first word extended to the longest prefix all matching commands share. */
export function completeCommand(line: string): string {
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
