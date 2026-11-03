// The clipboard, for :l48's y (design 6.6): pbcopy on macOS, else wl-copy
// or xclip, else OSC 52, which most terminals (and tmux with set-clipboard)
// put on the system clipboard themselves.

import { spawnSync } from "node:child_process";

/** Copies text; the way it went, or undefined when nothing took it. */
export type Clipboard = (text: string) => string | undefined;

const COMMANDS: readonly [string, readonly string[]][] = [
  ["pbcopy", []],
  ["wl-copy", []],
  ["xclip", ["-selection", "clipboard"]],
];

/** The system clipboard; `write` is the terminal, for OSC 52. */
export function systemClipboard(write: (s: string) => void = (s) => process.stdout.write(s)): Clipboard {
  return (text) => {
    for (const [cmd, args] of COMMANDS) {
      const r = spawnSync(cmd, [...args], { input: text, stdio: ["pipe", "ignore", "ignore"], timeout: 2000 });
      if (!r.error && r.status === 0) return cmd;
    }
    if (!process.stdout.isTTY) return undefined;
    write(`\x1b]52;c;${Buffer.from(text, "utf8").toString("base64")}\x07`);
    return "the terminal";
  };
}
