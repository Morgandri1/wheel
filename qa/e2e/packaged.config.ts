import { defineConfig, devices } from "@playwright/test";

/**
 * E2E against the PACKAGED board — what `npx wheel-web` actually starts.
 *
 * A SEPARATE config, not a project in playwright.config.ts, because Playwright starts every
 * configured `webServer` regardless of which project you select. Adding these servers there
 * meant a packaged run also booted both `next dev` servers it does not use, waited on them,
 * and could fail for reasons that have nothing to do with the package. One config per set of
 * servers is the only arrangement where `--project` means what it looks like it means.
 *
 * The package is pointed at a mock on :8789 while its build-time default is :8787. That gap
 * is the test: if `--api` were decorative — the natural outcome, since NEXT_PUBLIC_* values
 * freeze when the bundle compiles — the board would talk to 8787 and every assertion fails
 * on an empty page rather than passing quietly.
 *
 * Requires the package to exist: `make test-pkg` builds, packs and runs it.
 */
const PKG_URL = process.env.WHEEL_PKG_WEB_URL ?? "http://localhost:3300";
const PKG_API = process.env.WHEEL_PKG_API_URL ?? "http://localhost:8789";

export default defineConfig({
  testDir: "./tests",
  testMatch: /packaged\.spec\.ts/,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 1,
  reporter: process.env.CI
    ? [["list"], ["html", { outputFolder: "playwright-report", open: "never" }]]
    : "list",
  timeout: 60_000,
  expect: { timeout: 15_000 },
  use: {
    baseURL: PKG_URL,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
    ...devices["Desktop Chrome"],
  },
  webServer: [
    {
      command: "pnpm -C ../../web mock",
      url: `${PKG_API}/healthz`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
      env: { MOCK_PORT: "8789", MOCK_ORIGINS: PKG_URL },
    },
    {
      // The real entry point, run the way a user runs it: no build step, no NEXT_PUBLIC_*
      // in the environment, nothing but the flag.
      command: `node ../../web/dist-pkg/bin/wheel-web.mjs --port 3300 --api ${PKG_API}`,
      url: PKG_URL,
      // NEVER reuse. Every other suite may reuse a running dev server; this one must not,
      // because the thing under test IS how the server was launched. With reuse on, I
      // pointed the package at the wrong API to check the suite could fail, and it passed:
      // Playwright had found the previous server — started with the RIGHT flag — and used
      // that. The sabotage never reached the code, and a suite that cannot be made to fail
      // is not evidence of anything. Restarting is cheap; the bundle is prebuilt.
      reuseExistingServer: false,
      timeout: 120_000,
    },
  ],
});
