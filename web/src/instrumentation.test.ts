// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from "vitest";
import { register } from "./instrumentation";
import { CONFIG_EXIT_CODE } from "./instrumentation-node";

let exit: MockInstance<typeof process.exit>;
let log: MockInstance<typeof console.log>;
let error: MockInstance<typeof console.error>;

beforeEach(() => {
  vi.stubEnv("NEXT_RUNTIME", "nodejs");
  vi.stubEnv("WHEEL_API_URL", "http://api.internal:8080");
  vi.stubEnv("WHEEL_AUTH_MODE", "local");
  vi.stubEnv("WHEEL_PUBLIC_ORIGIN", "");
  vi.stubEnv("WHEEL_TRUST_PROXY", "");
  vi.stubEnv("VERCEL", "");
  vi.stubEnv("WHEEL_ALLOW_INSECURE_AUTH", "");
  exit = vi.spyOn(process, "exit").mockImplementation((() => undefined) as never);
  log = vi.spyOn(console, "log").mockImplementation(() => {});
  error = vi.spyOn(console, "error").mockImplementation(() => {});
});

afterEach(() => {
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
});

describe("starting the server", () => {
  it("says which API, auth mode and public origin it will use, and starts", async () => {
    await register();
    expect(log).toHaveBeenCalledWith("wheel-web: API http://api.internal:8080 · auth local · public origin: localhost only");
    expect(exit).not.toHaveBeenCalled();
  });

  it("refuses to start in production with the shared credential an unset WHEEL_AUTH_MODE means", async () => {
    vi.stubEnv("NODE_ENV", "production");
    vi.stubEnv("WHEEL_AUTH_MODE", "");
    await register();
    expect(error).toHaveBeenCalledWith(expect.stringMatching(/refusing to start.*WHEEL_ALLOW_INSECURE_AUTH/));
    expect(exit).toHaveBeenCalledWith(CONFIG_EXIT_CODE);
  });

  it("refuses to start with an API address that is not a URL", async () => {
    vi.stubEnv("WHEEL_API_URL", "api:8080 please");
    await register();
    expect(error).toHaveBeenCalledWith(expect.stringContaining("WHEEL_API_URL"));
    expect(exit).toHaveBeenCalledWith(CONFIG_EXIT_CODE);
  });

  it("does nothing on the edge runtime, which never serves the API routes", async () => {
    vi.stubEnv("NEXT_RUNTIME", "edge");
    vi.stubEnv("WHEEL_API_URL", "not a url");
    await register();
    expect(exit).not.toHaveBeenCalled();
    expect(log).not.toHaveBeenCalled();
  });
});
