import { build } from "esbuild";

await build({
  entryPoints: [process.argv[2] ?? "src/main.ts"],
  outfile: process.argv[3] ?? "dist/mcp.js",
  bundle: true,
  platform: "node",
  format: "esm",
  target: "node24",
  banner: {
    js: "#!/usr/bin/env node\nimport { createRequire as __createRequire } from 'node:module'; const require = __createRequire(import.meta.url);",
  },
  logLevel: "warning",
});
