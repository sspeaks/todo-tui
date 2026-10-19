import { build } from "esbuild";

// ink statically imports react-devtools-core for its optional DEV mode; stub it out.
const stubDevtools = {
  name: "stub-react-devtools-core",
  setup(b) {
    b.onResolve({ filter: /^react-devtools-core$/ }, () => ({ path: "stub", namespace: "stub" }));
    b.onLoad({ filter: /.*/, namespace: "stub" }, () => ({ contents: "export default {};", loader: "js" }));
  },
};

await build({
  entryPoints: [process.argv[2] ?? "src/main.tsx"],
  outfile: process.argv[3] ?? "dist/tui.js",
  bundle: true,
  platform: "node",
  format: "esm",
  target: "node24",
  jsx: "automatic",
  define: { "process.env.NODE_ENV": '"production"' },
  banner: {
    js: "#!/usr/bin/env node\nimport { createRequire as __createRequire } from 'node:module'; const require = __createRequire(import.meta.url);",
  },
  plugins: [stubDevtools],
  logLevel: "warning",
});
