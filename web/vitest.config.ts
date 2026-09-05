import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  test: {
    environment: "jsdom",
    coverage: {
      provider: "v8",
      reporter: ["text-summary", "lcov"],
      // §0b: 90% is a gate, not a warning. Scoped to the modules that encode rules — the wire
      // matrix, limits, delivery states and validation — because that is where being wrong is
      // silent. Components are covered by QA's Playwright suite; node-meta is a lookup table
      // with no branches, and asserting its contents against itself would be theatre.
      include: [
        "src/lib/wire-matrix.ts",
        "src/lib/limits.ts",
        "src/lib/message-state.ts",
        "src/lib/validate.ts",
      ],
      thresholds: { lines: 90, functions: 90, branches: 85, statements: 90 },
    },
    include: ["src/**/*.test.ts", "src/**/*.test.tsx", "mock/**/*.test.ts"],
  },
});
