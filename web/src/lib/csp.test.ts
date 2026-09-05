import { describe, expect, it } from "vitest";
import { buildCsp } from "./csp";

const base = { nonce: "abc123", apiUrl: "https://api.wheel.dev", authMode: "local", dev: false };
const parse = (policy: string) =>
  Object.fromEntries(policy.split("; ").map((d) => { const [k, ...v] = d.split(" "); return [k, v]; }));

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

  it("reaches the API over both http and the websocket, and nothing else", () => {
    expect(directives["connect-src"]).toEqual(["'self'", "https://api.wheel.dev", "wss://api.wheel.dev"]);
  });

  it("upgrades insecure requests", () => {
    expect(buildCsp(base)).toContain("upgrade-insecure-requests");
  });
});

describe("development", () => {
  it("allows eval and localhost, because the dev server needs them", () => {
    const directives = parse(buildCsp({ ...base, dev: true }));
    expect(directives["script-src"]).toContain("'unsafe-eval'");
    expect(directives["connect-src"]).toContain("ws://localhost:*");
  });

  it("never leaks that relaxation into a production policy", () => {
    expect(buildCsp(base)).not.toContain("localhost");
    expect(buildCsp(base)).not.toContain("unsafe-eval");
  });
});

describe("clerk mode", () => {
  it("admits Clerk's script and frames, and only in that mode", () => {
    const clerk = parse(buildCsp({ ...base, authMode: "clerk" }));
    expect(clerk["script-src"]).toContain("https://*.clerk.com");
    expect(clerk["frame-src"]).toContain("https://*.clerk.com");
    expect(parse(buildCsp(base))["frame-src"]).toEqual(["'none'"]);
  });
});

describe("a hostile NEXT_PUBLIC_API_URL", () => {
  it.each([
    ["a directive injection", "https://evil.example; script-src 'unsafe-inline'"],
    ["a space-separated second source", "https://evil.example https://also-evil.example"],
    ["a javascript: url", "javascript:alert(1)"],
    ["a data: url", "data:text/html,x"],
    ["nonsense", "not a url at all"],
  ])("cannot smuggle %s into the policy", (_name, apiUrl) => {
    const policy = buildCsp({ ...base, apiUrl });
    const directives = parse(policy);
    // script-src is the one that matters: style-src carries 'unsafe-inline' by design (see csp.ts).
    expect(directives["script-src"]).not.toContain("'unsafe-inline'");
    expect(policy).not.toContain("also-evil");
    expect(policy).not.toContain("javascript:");
    // Only the parsed origin survives, never the rest of the string.
    expect(directives["connect-src"]!.every((s) => !s.includes(";"))).toBe(true);
  });

  it("keeps only the origin when a path or query is attached", () => {
    const policy = buildCsp({ ...base, apiUrl: "https://api.wheel.dev/v1/things?x=1" });
    expect(parse(policy)["connect-src"]).toEqual(["'self'", "https://api.wheel.dev", "wss://api.wheel.dev"]);
  });

  it("still produces a usable policy when the API url is missing entirely", () => {
    const directives = parse(buildCsp({ ...base, apiUrl: undefined }));
    expect(directives["connect-src"]).toEqual(["'self'"]);
    expect(directives["default-src"]).toEqual(["'self'"]);
  });
});
