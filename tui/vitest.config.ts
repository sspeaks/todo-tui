import { defineConfig } from "vitest/config";

// Colors on, so tests can see the new-row highlight.
export default defineConfig({ test: { env: { FORCE_COLOR: "1" } } });
