import { defineConfig, devices } from "@playwright/test";

/**
 * E2E against Web's mock API (`pnpm mock`, port 8787) plus `next dev` on 3000.
 *
 * The mock is a real HTTP+WS server that enforces the §3 wire matrix server-side and
 * 404s projects it does not own, so the failure paths are genuinely exercised rather
 * than stubbed in the browser. Web offered it; using it beats standing up the whole
 * Rust stack for UI assertions.
 *
 * No retries. A flaky gate that goes green on retry is a gate people learn to ignore;
 * a flake here is a bug in the test or the app and should be fixed, not re-rolled.
 */
export default defineConfig({
  testDir: ".",
  testMatch: /.*\.spec\.ts/,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 1,
  reporter: process.env.CI ? [["list"], ["html", { outputFolder: "playwright-report", open: "never" }]] : "list",
  timeout: 30_000,
  expect: { timeout: 10_000 },
  use: {
    baseURL: process.env.WHEEL_WEB_URL ?? "http://localhost:3000",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: process.env.WHEEL_WEB_URL
    ? undefined
    : {
        command: "pnpm -C ../../web dev:mock",
        url: "http://localhost:3000",
        reuseExistingServer: !process.env.CI,
        timeout: 180_000,
        env: {
          NEXT_PUBLIC_API_URL: "http://localhost:8787",
          NEXT_PUBLIC_AUTH_MODE: "mock",
        },
      },
});
