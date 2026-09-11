// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, describe, expect, it, vi } from "vitest";
import { buildCsp } from "./csp";

const base = { nonce: "abc123", authMode: "local", dev: false };
const parse = (policy: string): Record<string, string[]> =>
  Object.fromEntries(policy.split("; ").map((d) => { const [k, ...v] = d.split(" "); return [k!, v]; }));

describe("the production policy", () => {
  const directives = parse(buildCsp(base));

  it("allows no inline script and no eval", () => {
    expect(directives["script-src"]).toContain("'nonce-abc123'");
    expect(directives["script-src"]).not.toContain("'unsafe-inline'");
    expect(directives["script-src"]).not.toContain("'unsafe-eval'");
  });

  it.each([
    ["object-src", "'none'"],
    ["base-uri", "'none'"],
    ["frame-ancestors", "'none'"],
    ["frame-src", "'none'"],
    ["form-action", "'self'"],
  ])("locks down %s", (directive, value) => {
    expect(directives[directive]).toEqual([value]);
  });

  it("lets the page connect to this origin and nowhere else", () => {
    expect(directives["connect-src"]).toEqual(["'self'"]);
  });

  it("upgrades insecure requests", () => {
    expect(buildCsp(base)).toContain("upgrade-insecure-requests");
  });
});

describe("the API's address", () => {
  afterEach(() => vi.unstubAllEnvs());

  // The policy is never told where the API is, so it cannot publish it — whatever the env says.
  it.each(["mock", "dev", "local", "clerk"])("never appears in a %s-mode policy", (authMode) => {
    vi.stubEnv("WHEEL_API_URL", "http://sentinel-api.internal:8080");
    vi.stubEnv("NEXT_PUBLIC_API_URL", "https://sentinel-public.example");
    for (const dev of [false, true]) {
      const policy = buildCsp({ ...base, authMode, dev });
      expect(policy).not.toContain("sentinel");
      expect(policy).not.toContain(":8080");
    }
  });
});

describe("development", () => {
  it("allows eval and the dev server's hot-reload socket, because the dev server needs them", () => {
    const directives = parse(buildCsp({ ...base, dev: true }));
    expect(directives["script-src"]).toContain("'unsafe-eval'");
    expect(directives["connect-src"]).toEqual(["'self'", "ws://localhost:*", "ws://127.0.0.1:*"]);
  });

  it("drops strict-dynamic so Next's error overlay is readable", () => {
    // With strict-dynamic, 'self' means nothing and the overlay's un-nonced fallback chunks are
    // refused — a missing module then renders as a blank page instead of an error.
    const dev = parse(buildCsp({ ...base, dev: true }));
    expect(dev["script-src"]).not.toContain("'strict-dynamic'");
    expect(dev["script-src"]).toContain("'self'");
    expect(parse(buildCsp(base))["script-src"]).toContain("'strict-dynamic'");
  });

  it("never leaks that relaxation into a production policy", () => {
    expect(buildCsp(base)).not.toContain("localhost");
    expect(buildCsp(base)).not.toContain("127.0.0.1");
    expect(buildCsp(base)).not.toContain("unsafe-eval");
  });
});

describe("clerk mode", () => {
  it("admits Clerk's script, frames and API, and only in that mode", () => {
    const clerk = parse(buildCsp({ ...base, authMode: "clerk" }));
    expect(clerk["script-src"]).toContain("https://*.clerk.com");
    expect(clerk["frame-src"]).toContain("https://*.clerk.com");
    expect(clerk["connect-src"]).toEqual(["'self'", "https://*.clerk.accounts.dev", "https://*.clerk.com"]);
    const local = parse(buildCsp(base));
    expect(local["frame-src"]).toEqual(["'none'"]);
    expect(local["connect-src"]).toEqual(["'self'"]);
  });
});
