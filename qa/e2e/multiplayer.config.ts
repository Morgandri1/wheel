import { defineConfig, devices } from "@playwright/test";
import os from "node:os";
import path from "node:path";

/**
 * E2E against the REAL wheel-api (sqlite, local auth) and the PACKAGED web, with only the sandbox host stubbed
 * (support/stub-host.mjs). Multiplayer is enforced in the API, so a mock API cannot say anything about it: the
 * mock in web/mock has no tiers, no members and no invites, and a test against it would test the mock.
 *
 * Needs a built API binary: WHEEL_API_BIN, default the shared cargo target's debug binary. `make test-multiplayer`
 * builds it.
 */
const WEB = process.env.WHEEL_MP_WEB_URL ?? "http://127.0.0.1:3301";
const API_PORT = 8790;
const HOST_PORT = 8791;
const HOST_SECRET = "stub-host-secret-0123456789";
const API_BIN =
  process.env.WHEEL_API_BIN ?? path.join(process.env.CARGO_TARGET_DIR ?? path.resolve(__dirname, "../../target"), "debug/wheel-api");
const DB = path.join(os.tmpdir(), `wheel-mp-${process.pid}.db`);

export default defineConfig({
  testDir: "./tests",
  testMatch: /real-api\.spec\.ts/,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 1,
  reporter: process.env.CI ? [["list"], ["html", { outputFolder: "playwright-report", open: "never" }]] : "list",
  timeout: 60_000,
  expect: { timeout: 15_000 },
  use: {
    baseURL: WEB,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    ...devices["Desktop Chrome"],
  },
  webServer: [
    {
      command: "node support/stub-host.mjs",
      url: `http://127.0.0.1:${HOST_PORT}/healthz`,
      reuseExistingServer: false,
      timeout: 30_000,
      env: { STUB_HOST_PORT: String(HOST_PORT), STUB_HOST_SECRET: HOST_SECRET },
    },
    {
      command: API_BIN,
      url: `http://127.0.0.1:${API_PORT}/healthz`,
      reuseExistingServer: false,
      timeout: 60_000,
      env: {
        WHEEL_ENV: "prod",
        AUTH_MODE: "local",
        WHEEL_SIGNUP: "open",
        STORE: `sqlite://${DB}?mode=rwc`,
        API_MASTER_KEY: Buffer.alloc(32, 7).toString("base64"),
        WHEEL_HOST_URL: `http://127.0.0.1:${HOST_PORT}`,
        WHEEL_HOST_SECRET: HOST_SECRET,
        BIND_ADDR: `127.0.0.1:${API_PORT}`,
        PUBLIC_BASE_URL: `http://127.0.0.1:${API_PORT}`,
      },
    },
    {
      command: `node ../../web/dist-pkg/bin/wheel-web.mjs --port 3301 --api http://127.0.0.1:${API_PORT}`,
      url: WEB,
      reuseExistingServer: false,
      timeout: 120_000,
    },
  ],
});
