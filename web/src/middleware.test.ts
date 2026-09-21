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
 * (HOSTNAME=127.0.0.1, PORT=3000) and only the headers carry the public host, so an absolute URL
 * built from `req.url` points browsers at the server's own loopback. The base is `publicOrigin()`,
 * the derivation every /api route already uses.
 *
 * NOT covered here, and it matters: these call `middleware()` directly, so they never reach Next's
 * middleware adapter. A relative `Location` passed every one of them and still answered 500 on a
 * real server (the adapter parses it as absolute). `scripts/probe-redirect.sh` builds and probes
 * a real standalone server; run it for anything that changes what middleware returns.
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

const locationOf = (res: Response) => res.headers.get("location") ?? "";

describe("the signed-out redirect, behind a proxy", () => {
  describe("with WHEEL_PUBLIC_ORIGIN set", () => {
    beforeEach(() => {
      vi.stubEnv("WHEEL_AUTH_MODE", "local");
      vi.stubEnv("WHEEL_PUBLIC_ORIGIN", "https://wheel.avo.so");
      vi.stubEnv("WHEEL_TRUST_PROXY", "1");
    });

    it.each([
      ["/app", "https://wheel.avo.so/sign-in"],
      ["/app/9b1d-44", "https://wheel.avo.so/sign-in?next=%2Fapp%2F9b1d-44"],
      ["/app/invite/wi_abc", "https://wheel.avo.so/sign-in?next=%2Fapp%2Finvite%2Fwi_abc"],
    ])("sends %s to %s, never the server's own address", async (path, expected) => {
      const res = await middleware(behindProxy(path), ev);
      expect(res.status).toBe(307);
      expect(locationOf(res)).toBe(expected);
    });

    it("cannot be steered by forwarded headers: a configured origin wins outright", async () => {
      const res = await middleware(
        behindProxy("/app", { "x-forwarded-host": "evil.example", host: "evil.example", "x-forwarded-proto": "http" }),
        ev,
      );
      expect(locationOf(res)).toBe("https://wheel.avo.so/sign-in");
    });

    it("keeps an awkward path inside ?next= rather than letting it leave the origin", async () => {
      for (const path of ["/app/%5Cevil.com", "/app/..%2Fx", "/app/%2F%2Fevil.com"]) {
        const url = new URL(locationOf(await middleware(behindProxy(path), ev)));
        expect(url.origin).toBe("https://wheel.avo.so");
        expect(url.pathname).toBe("/sign-in");
      }
    });

    it("carries the document CSP on the redirect, as before", async () => {
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

  it("with only WHEEL_TRUST_PROXY, follows the proxy's forwarded host and scheme", async () => {
    vi.stubEnv("WHEEL_AUTH_MODE", "local");
    vi.stubEnv("WHEEL_TRUST_PROXY", "1");
    expect(locationOf(await middleware(behindProxy("/app"), ev))).toBe("https://wheel.avo.so/sign-in");
  });

  it("with neither set (localhost-only mode), stays on the request's own origin", async () => {
    vi.stubEnv("WHEEL_AUTH_MODE", "local");
    const res = await middleware(
      new NextRequest(new Request("http://localhost:3000/app", { headers: { host: "localhost:3000" } })),
      ev,
    );
    expect(locationOf(res)).toBe("http://localhost:3000/sign-in");
  });
});
