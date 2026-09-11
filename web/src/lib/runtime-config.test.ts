// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_API_URL,
  devToken,
  parseAuthMode,
  proxyBodyLimit,
  publicOriginSetting,
  serverApiBaseUrl,
  serverAuthMode,
  trustProxy,
} from "./runtime-config";

function env(vars: Record<string, string | undefined>) {
  for (const [k, v] of Object.entries(vars)) vi.stubEnv(k, v ?? "");
}

afterEach(() => vi.unstubAllEnvs());

/**
 * Everything here is read per call, on the server, so a prebuilt package or image follows the
 * environment it is started in. If these pass while resolution silently falls back to a baked
 * constant, `npx wheel-web --api …` ships with an option that does nothing.
 */
describe("serverApiBaseUrl", () => {
  it("defaults to loopback on the API's port, so an unconfigured server reaches nothing off the machine", () => {
    env({ WHEEL_API_URL: undefined, NEXT_PUBLIC_API_URL: undefined });
    expect(serverApiBaseUrl()).toBe("http://127.0.0.1:8080");
    expect(DEFAULT_API_URL).toBe("http://127.0.0.1:8080");
  });

  it("prefers WHEEL_API_URL, the runtime knob", () => {
    env({ WHEEL_API_URL: "https://api.example.test", NEXT_PUBLIC_API_URL: "https://baked.test" });
    expect(serverApiBaseUrl()).toBe("https://api.example.test");
  });

  it("falls back to the NEXT_PUBLIC_API_URL an existing deployment already sets", () => {
    env({ WHEEL_API_URL: undefined, NEXT_PUBLIC_API_URL: "https://baked.test" });
    expect(serverApiBaseUrl()).toBe("https://baked.test");
  });

  it("treats a blank WHEEL_API_URL as unset rather than as a relative base", () => {
    env({ WHEEL_API_URL: "   ", NEXT_PUBLIC_API_URL: "https://baked.test" });
    expect(serverApiBaseUrl()).toBe("https://baked.test");
  });

  it("keeps a base path but drops trailing slashes, query and fragment", () => {
    env({ WHEEL_API_URL: "https://api.example.test/base//?x=1#frag" });
    expect(serverApiBaseUrl()).toBe("https://api.example.test/base");
  });

  it.each(["not a url", "ftp://api.example.test", "javascript:alert(1)"])("refuses %j loudly", (value) => {
    env({ WHEEL_API_URL: value });
    expect(() => serverApiBaseUrl()).toThrow(/WHEEL_API_URL must be an http\(s\) URL/);
  });
});

describe("serverAuthMode", () => {
  it("reads WHEEL_AUTH_MODE at run time", () => {
    env({ WHEEL_AUTH_MODE: "local", NEXT_PUBLIC_AUTH_MODE: "clerk" });
    expect(serverAuthMode()).toBe("local");
  });

  it("falls back to NEXT_PUBLIC_AUTH_MODE for deployments configured before WHEEL_AUTH_MODE", () => {
    env({ WHEEL_AUTH_MODE: undefined, NEXT_PUBLIC_AUTH_MODE: "clerk" });
    expect(serverAuthMode()).toBe("clerk");
  });

  it("is mock when neither is set, as it always was", () => {
    env({ WHEEL_AUTH_MODE: undefined, NEXT_PUBLIC_AUTH_MODE: undefined });
    expect(serverAuthMode()).toBe("mock");
  });
});

describe("parseAuthMode", () => {
  it.each(["mock", "dev", "local", "clerk"] as const)("accepts %s", (mode) => {
    expect(parseAuthMode(mode)).toBe(mode);
  });

  it("forgives surrounding whitespace, which no dashboard shows", () => {
    expect(parseAuthMode(" local ")).toBe("local");
  });

  it("treats unset as mock", () => {
    expect(parseAuthMode(undefined)).toBe("mock");
  });

  // The old footgun: a typo rendered the sign-in page, "succeeded", then 401'd forever.
  it.each(["locol", "Local", "jwks", "local mode"])("refuses %j instead of failing silently later", (raw) => {
    expect(() => parseAuthMode(raw)).toThrow(/is not one of: mock, dev, local, clerk/);
  });
});

describe("publicOriginSetting", () => {
  it("is unset by default", () => {
    env({ WHEEL_PUBLIC_ORIGIN: undefined });
    expect(publicOriginSetting()).toBeNull();
  });

  it("normalises to a bare origin", () => {
    env({ WHEEL_PUBLIC_ORIGIN: "https://Wheel.Example.com/" });
    expect(publicOriginSetting()).toBe("https://wheel.example.com");
  });

  it.each(["wheel.example.com", "ftp://wheel.example.com", "https://wheel.example.com/app", "https://wheel.example.com/?x=1"])(
    "refuses %j loudly — it has to be an origin and nothing more",
    (value) => {
      env({ WHEEL_PUBLIC_ORIGIN: value });
      expect(() => publicOriginSetting()).toThrow(/WHEEL_PUBLIC_ORIGIN must be an origin/);
    },
  );
});

describe("trustProxy", () => {
  it.each<[string | undefined, string | undefined, boolean]>([
    [undefined, undefined, false],
    ["1", undefined, true],
    ["true", undefined, true],
    ["TRUE", undefined, true],
    ["0", undefined, false],
    ["no", undefined, false],
    // Vercel's edge always sets the forwarded headers, and a function cannot be reached around it.
    [undefined, "1", true],
    ["0", "1", false],
  ])("WHEEL_TRUST_PROXY=%j with VERCEL=%j → %s", (value, vercel, expected) => {
    env({ WHEEL_TRUST_PROXY: value, VERCEL: vercel });
    expect(trustProxy()).toBe(expected);
  });
});

describe("devToken", () => {
  it("comes from WHEEL_DEV_TOKEN", () => {
    env({ WHEEL_DEV_TOKEN: "minted.jwt.value" });
    expect(devToken()).toBe("minted.jwt.value");
  });

  it("never from the NEXT_PUBLIC_ name, which would put it in the bundle", () => {
    env({ WHEEL_DEV_TOKEN: undefined, NEXT_PUBLIC_DEV_TOKEN: "leaked" });
    expect(devToken()).toBeNull();
  });
});

describe("proxyBodyLimit", () => {
  it("defaults to the API's own 5 MiB ingress cap", () => {
    env({ WHEEL_PROXY_BODY_LIMIT_BYTES: undefined });
    expect(proxyBodyLimit()).toBe(5 * 1024 * 1024);
  });

  it("follows WHEEL_PROXY_BODY_LIMIT_BYTES", () => {
    env({ WHEEL_PROXY_BODY_LIMIT_BYTES: "1024" });
    expect(proxyBodyLimit()).toBe(1024);
  });

  it.each(["-5", "0", "1.5", "lots"])("ignores %j rather than disabling the cap", (value) => {
    env({ WHEEL_PROXY_BODY_LIMIT_BYTES: value });
    expect(proxyBodyLimit()).toBe(5 * 1024 * 1024);
  });
});
