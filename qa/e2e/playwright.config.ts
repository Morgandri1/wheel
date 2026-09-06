import { defineConfig, devices } from "@playwright/test";

/**
 * E2E against Web's mock API (`pnpm mock`) plus `next dev`.
 *
 * The mock is a real HTTP+WS server that enforces the §3 wire matrix server-side and
 * 404s projects it does not own, so the failure paths are genuinely exercised rather
 * than stubbed in the browser. Web offered it; using it beats standing up the whole
 * Rust stack for UI assertions.
 *
 * TWO SERVERS, because `NEXT_PUBLIC_AUTH_MODE` is inlined at build time and one server
 * can only be built for one mode:
 *   - `chromium`   :3000 / mock :8787 — mock auth mode; the board, agent and vault specs.
 *   - `local-auth` :3200 / mock :8788 — AUTH_MODE=local; the sign-in/sign-up specs.
 * They get separate `.next` caches via NEXT_DIST_DIR (Web added that for exactly this),
 * so neither corrupts the other's build. The alternative Web offered — run local auth by
 * hand before a deploy — is not a gate: AUTH-local-* carries S1 criteria, including the
 * enumeration oracle a client can reintroduce on its own, and an S1 nobody runs is an S1
 * nobody catches.
 *
 * No retries. A flaky gate that goes green on retry is a gate people learn to ignore;
 * a flake here is a bug in the test or the app and should be fixed, not re-rolled.
 */
const LOCAL_AUTH = /local-auth\.spec\.ts/;
// packaged.spec.ts runs under its own config (packaged.config.ts) with its own servers.
// It is ignored here so a normal run does not try to load the packaged board off a dev
// server that was never built for it.
const PACKAGED = /packaged\.spec\.ts/;

const WEB_URL = process.env.WHEEL_WEB_URL ?? "http://localhost:3000";
const LOCAL_URL = process.env.WHEEL_LOCAL_WEB_URL ?? "http://localhost:3200";
const LOCAL_API = process.env.WHEEL_LOCAL_API_URL ?? "http://localhost:8788";

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
    baseURL: WEB_URL,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      testIgnore: [LOCAL_AUTH, PACKAGED],
      use: { ...devices["Desktop Chrome"] },
    },
    {
      name: "local-auth",
      testMatch: LOCAL_AUTH,
      use: { ...devices["Desktop Chrome"], baseURL: LOCAL_URL },
    },
  ],
  webServer: process.env.WHEEL_WEB_URL
    ? undefined
    : [
        {
          command: "pnpm -C ../../web dev:mock",
          url: WEB_URL,
          reuseExistingServer: !process.env.CI,
          timeout: 180_000,
          env: {
            NEXT_PUBLIC_API_URL: "http://localhost:8787",
            NEXT_PUBLIC_AUTH_MODE: "mock",
          },
        },
        {
          command: "pnpm -C ../../web mock",
          url: `${LOCAL_API}/healthz`,
          reuseExistingServer: !process.env.CI,
          timeout: 60_000,
          env: {
            MOCK_PORT: "8788",
            MOCK_ORIGINS: LOCAL_URL,
          },
        },
        {
          command: "pnpm -C ../../web exec next dev --port 3200",
          url: LOCAL_URL,
          reuseExistingServer: !process.env.CI,
          timeout: 180_000,
          env: {
            NEXT_DIST_DIR: ".next-local",
            NEXT_PUBLIC_API_URL: LOCAL_API,
            NEXT_PUBLIC_AUTH_MODE: "local",
          },
        },
      ],
});
