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
