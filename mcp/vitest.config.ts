import { defineConfig } from "vitest/config";

// A fixed zone, so dates do not depend on the machine.
export default defineConfig({ test: { env: { TZ: "UTC" } } });
