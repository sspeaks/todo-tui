import { defineConfig } from "vitest/config";

// Colors on, so tests can see the new-row highlight; a fixed zone, so local
// times and dates do not depend on the machine; and a fixed terminal size
// for anything that falls back to it (the render tests size their own).
export default defineConfig({ test: { env: { FORCE_COLOR: "1", TZ: "UTC", COLUMNS: "100", LINES: "30" } } });
