// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { NextRequest } from "next/server";
import type { NextFetchEvent } from "next/server";
import middleware from "./middleware";
import { returnedResponseHeaders } from "@/lib/proxy-rules";

/**
 * QA review round 2: middleware sets the document Content-Security-Policy on every response,
 * including `/api/wheel/*`, and Next applies middleware's response headers LAST — so it was
 * silently overwriting the `sandbox` policy `proxy-rules.ts` puts on a non-JSON proxied body
 * (a chest blob, say), which is exactly what stops that body running as a document with the
 * user's session if someone opens its raw URL. `ev` is never read outside clerk mode, which
 * these tests never enter, so a bare object stands in for it.
 */
const ev = {} as NextFetchEvent;

function req(url: string) {
  return new NextRequest(new Request(url, { headers: { host: "wheel.test" } }));
}

beforeEach(() => {
  vi.stubEnv("WHEEL_AUTH_MODE", "mock");
});

afterEach(() => vi.unstubAllEnvs());

describe("the document CSP", () => {
  it("is set on an ordinary page or JSON API response", async () => {
    const res = await middleware(req("https://wheel.test/app"), ev);
    expect(res.headers.get("content-security-policy")).toContain("default-src 'self'");
  });

  it("is set on session routes, which build their own JSON and never need sandbox", async () => {
    const res = await middleware(req("https://wheel.test/api/session"), ev);
    expect(res.headers.get("content-security-policy")).toContain("default-src 'self'");
  });

  it("is left unset on /api/wheel/* so the route's own header cannot be overwritten", async () => {
    const paths = [
      "https://wheel.test/api/wheel/v1/projects/p1/engine/v1/chests/c1/blob",
      "https://wheel.test/api/wheel/probe",
      "https://wheel.test/api/wheel/projects/p1/events",
    ];
    for (const url of paths) {
      const res = await middleware(req(url), ev);
      expect(res.headers.get("content-security-policy")).toBeNull();
    }
  });

  // The actual defense: with middleware out of the way, the route's own `sandbox` policy for a
  // non-JSON proxied body is what a browser would actually receive. Proven directly against the
  // function real responses go through, not re-implemented here.
  it("would otherwise have replaced the sandbox policy a proxied chest blob relies on", () => {
    const routeHeaders = returnedResponseHeaders(new Headers({ "content-type": "text/html" }));
    expect(routeHeaders.get("content-security-policy")).toBe("sandbox");
  });
});

/**
 * P0 (wheel.avo.so): a signed-out visit to /app answered `Location: https://localhost:3000/sign-in`.
 * Behind Caddy, Next's standalone server builds `req.url` from its own bind address
 * (HOSTNAME=127.0.0.1, PORT=3000), and only the headers carry the public host — so any absolute
 * URL built from `req.url` points the browser at the server's own loopback. The redirect is
 * therefore relative: the browser resolves it against the URL IT used, with no host derivation and
 * no forwarded header able to steer it (nothing to open-redirect through).
 */
function behindProxy(path: string, headers: Record<string, string> = {}) {
  return new NextRequest(
    new Request(`http://localhost:3000${path}`, {
      headers: {
        host: "wheel.avo.so",
        "x-forwarded-host": "wheel.avo.so",
        "x-forwarded-proto": "https",
        ...headers,
      },
    }),
  );
}

describe("the signed-out redirect, behind a proxy", () => {
  beforeEach(() => {
    vi.stubEnv("WHEEL_AUTH_MODE", "local");
    vi.stubEnv("WHEEL_PUBLIC_ORIGIN", "https://wheel.avo.so");
    vi.stubEnv("WHEEL_TRUST_PROXY", "1");
  });

  it.each([
    ["/app", "/sign-in"],
    ["/app/9b1d-44", "/sign-in?next=%2Fapp%2F9b1d-44"],
    ["/app/invite/wi_abc", "/sign-in?next=%2Fapp%2Finvite%2Fwi_abc"],
  ])("sends %s to %s without ever naming the server's own address", async (path, expected) => {
    const res = await middleware(behindProxy(path), ev);
    expect(res.status).toBe(307);
    const location = res.headers.get("location") ?? "";
    expect(location).toBe(expected);
    expect(location).not.toMatch(/localhost|127\.0\.0\.1|:3000/);
  });

  it("stays relative whatever the forwarded headers claim — they cannot steer it", async () => {
    const res = await middleware(
      behindProxy("/app", { "x-forwarded-host": "evil.example", host: "evil.example", "x-forwarded-proto": "http" }),
      ev,
    );
    const location = res.headers.get("location") ?? "";
    expect(location).toBe("/sign-in");
    expect(location.startsWith("//")).toBe(false);
  });

  it("carries the document CSP on the redirect, as it did before", async () => {
    const res = await middleware(behindProxy("/app"), ev);
    expect(res.headers.get("content-security-policy")).toContain("default-src 'self'");
  });

  it("does not redirect a visitor whose session cookie is live", async () => {
    const b = (o: object) => Buffer.from(JSON.stringify(o)).toString("base64url");
    const jwt = `${b({ alg: "HS256" })}.${b({ sub: "u1", exp: Math.floor(Date.now() / 1000) + 600 })}.x`;
    const res = await middleware(behindProxy("/app", { cookie: `__Host-wheel_session=${jwt}` }), ev);
    expect(res.headers.get("location")).toBeNull();
  });
});
