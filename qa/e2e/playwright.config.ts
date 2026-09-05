import { defineConfig, devices } from "@playwright/test";

/**
 * E2E config. baseURL and the API both come from env so the same suite can run against
 * Web's `pnpm mock` (fast, hermetic) or a real docker-compose stack.
 *
 * retries: 0 deliberately. A retried flaky test is a test that reports success for a
 * product that intermittently fails, and the retry hides exactly the race a user would
 * hit. Flakes get quarantined and fixed, not re-run until green.
 */
export default defineConfig({
  testDir: "./tests",
  timeout: 30_000,
  expect: { timeout: 10_000 },
  retries: 0,
  workers: process.env.CI ? 2 : undefined,
  forbidOnly: !!process.env.CI,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : [["list"]],
  use: {
    baseURL: process.env.WEB_URL || "http://localhost:3000",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
