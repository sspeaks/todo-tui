// The phone app bundles "./portable" and "./react": nothing they reach may
// need Node. Follows their relative imports and checks each file's source.
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const SRC = join(dirname(fileURLToPath(import.meta.url)), "../src");
const IMPORT = /(?:^|\n)\s*(?:import|export)\s[^;]*?from\s+"([^"]+)"/g;
const NODE_ONLY = [/from\s+"node:/, /\bBuffer\./, /\bprocess\./, /from\s+"@connectrpc\/connect-node"/];

function reach(entry: string): Map<string, string> {
  const seen = new Map<string, string>();
  const todo = [resolve(SRC, entry)];
  while (todo.length > 0) {
    const file = todo.pop()!;
    if (seen.has(file)) continue;
    const text = readFileSync(file, "utf8");
    seen.set(file, text);
    for (const m of text.matchAll(IMPORT)) {
      const spec = m[1]!;
      if (spec.startsWith(".")) todo.push(resolve(dirname(file), spec.replace(/\.js$/, ".ts")));
    }
  }
  return seen;
}

describe.each(["portable.ts", "hooks.ts"])("%s", (entry) => {
  it("reaches no Node API", () => {
    const files = reach(entry);
    expect(files.size).toBeGreaterThan(1);
    for (const [file, text] of files) {
      for (const bad of NODE_ONLY) expect(bad.test(text), `${file.slice(SRC.length + 1)} matches ${bad}`).toBe(false);
    }
  });
});
