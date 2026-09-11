import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
      "server-only": fileURLToPath(new URL("./test/server-only.ts", import.meta.url)),
    },
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./test/setup.ts"],
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
        "src/lib/local-auth.ts",
        "src/lib/auth-session.ts",
        "src/lib/csp.ts",
        "src/lib/events.ts",
        "src/lib/message-state.ts",
        "src/lib/validate.ts",
        // What a status code is allowed to claim. Being wrong here is silent and lands on the
        // operator: a bare 404 read as "your path is wrong" cost an hour on /tg.
        "src/lib/drag-overrides.ts",
        "src/lib/auth-mode-check.ts",
        "src/lib/agent-config.ts",
        "src/lib/edge-geometry.ts",
        "src/lib/password.ts",
        "src/components/board/sidebar-rail.tsx",
        "src/components/builder/builder-panel.tsx",
        "src/lib/board-apply.ts",
        "src/lib/endpoint-probe.ts",
        "src/lib/workflow-proposal.ts",
        "src/lib/wired-vaults.ts",
        "src/lib/endpoint-reach.ts",
        "src/lib/templates.ts",
        "src/components/templates/template-gallery.tsx",
        // The server-side trust boundary: which paths, headers and bodies reach the API, which
        // requests count as same-origin, the session cookie's flags and lifetime, which credential
        // each mode presents, and the SSE relay. A wrong branch in any of these is silent.
        "src/lib/runtime-config.ts",
        "src/lib/same-origin.ts",
        "src/lib/session-cookie.ts",
        "src/lib/proxy-rules.ts",
        "src/lib/upstream.ts",
        "src/lib/api-proxy.ts",
        "src/lib/session-routes.ts",
        "src/lib/ingress-probe.ts",
        "src/lib/event-relay.ts",
      ],
      thresholds: { lines: 90, functions: 90, branches: 85, statements: 90 },
    },
    include: ["src/**/*.test.ts", "src/**/*.test.tsx", "mock/**/*.test.ts"],
  },
});
