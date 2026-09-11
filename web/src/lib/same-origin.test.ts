// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  clientAddress,
  hostAllowed,
  isLoopbackHost,
  isSameOrigin,
  publicOrigin,
  refuseCrossOrigin,
  requestOrigin,
  type ProxyTrust,
  type RequestOrigin,
} from "./same-origin";

const NO_TRUST: ProxyTrust = { publicOrigin: null, trustProxy: false };
const TRUSTED: ProxyTrust = { publicOrigin: null, trustProxy: true };
const CONFIGURED: ProxyTrust = { publicOrigin: "https://wheel.example.com", trustProxy: false };

beforeEach(() => {
  vi.stubEnv("WHEEL_PUBLIC_ORIGIN", "");
  vi.stubEnv("WHEEL_TRUST_PROXY", "");
  vi.stubEnv("VERCEL", "");
});

afterEach(() => {
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
});

function req(url: string, headers: Record<string, string>, method = "POST") {
  return new Request(url, { method, headers });
}

describe("the public origin", () => {
  it("on a direct http connection is the Host the browser used", () => {
    expect(publicOrigin(req("http://localhost:3000/api/x", { host: "localhost:3000" }), NO_TRUST)).toBe(
      "http://localhost:3000",
    );
  });

  it("behind a trusted proxy is what the NEAREST proxy reports: the rightmost value", () => {
    const behind = req("http://0.0.0.0:3000/api/x", {
      host: "web:3000",
      "x-forwarded-proto": "http, https",
      "x-forwarded-host": "spoofed.example, Wheel.Example.com",
    });
    expect(publicOrigin(behind, TRUSTED)).toBe("https://wheel.example.com");
  });

  it("does not let a value a client prepended win", () => {
    const behind = req("http://0.0.0.0:3000/api/x", {
      host: "web:3000",
      "x-forwarded-proto": "https, http",
      "x-forwarded-host": "wheel.example.com, inner",
    });
    expect(publicOrigin(behind, TRUSTED)).toBe("http://inner");
  });

  it("uses the Host a trusted proxy passed through when it sends no X-Forwarded-Host", () => {
    const behind = req("http://0.0.0.0:3000/api/x", { host: "wheel.example.com", "x-forwarded-proto": "https" });
    expect(publicOrigin(behind, TRUSTED)).toBe("https://wheel.example.com");
  });

  it("ignores forwarded headers from a peer nobody declared trusted — and does not believe the URL's scheme either", () => {
    const forged = req("https://wheel.example.com:3000/api/x", {
      host: "wheel.example.com:3000",
      "x-forwarded-proto": "https",
      "x-forwarded-host": "evil.example",
    });
    expect(publicOrigin(forged, NO_TRUST)).toBe("http://wheel.example.com:3000");
  });

  it("is WHEEL_PUBLIC_ORIGIN when set, whatever any header says", () => {
    const anything = req("http://0.0.0.0:3000/api/x", {
      host: "web:3000",
      "x-forwarded-proto": "http",
      "x-forwarded-host": "evil.example",
    });
    expect(publicOrigin(anything, { publicOrigin: "https://wheel.example.com", trustProxy: true })).toBe(
      "https://wheel.example.com",
    );
  });

  it("is 'null' when the host cannot be parsed, which no Origin can match", () => {
    expect(publicOrigin(req("http://web:3000/x", { host: "bad host" }), NO_TRUST)).toBe("null");
  });

  it("reads WHEEL_PUBLIC_ORIGIN and WHEEL_TRUST_PROXY from the environment by default", () => {
    vi.stubEnv("WHEEL_TRUST_PROXY", "1");
    const behind = req("http://web:3000/x", { host: "web:3000", "x-forwarded-proto": "https", "x-forwarded-host": "a.example" });
    expect(publicOrigin(behind)).toBe("https://a.example");
    vi.stubEnv("WHEEL_PUBLIC_ORIGIN", "https://b.example/");
    expect(publicOrigin(behind)).toBe("https://b.example");
  });
});

describe("DNS rebinding", () => {
  it.each<[string, boolean]>([
    ["localhost:3000", true],
    ["localhost", true],
    ["127.0.0.1:3000", true],
    ["127.9.9.9", true],
    ["[::1]:3000", true],
    ["wheel.example.com", false],
    ["127.0.0.1.evil.example", false],
    ["localhost.evil.example", false],
    ["10.0.0.5:3000", false],
    ["bad host", false],
  ])("treats Host %j as loopback: %s", (host, loopback) => {
    expect(isLoopbackHost(host)).toBe(loopback);
  });

  it("answers only loopback while no public origin is configured and no proxy is trusted", () => {
    expect(hostAllowed(req("http://localhost:3000/x", { host: "localhost:3000" }), NO_TRUST)).toBe(true);
    expect(hostAllowed(req("http://rebound.example:3000/x", { host: "rebound.example:3000" }), NO_TRUST)).toBe(false);
  });

  it("answers any Host once the public origin is configured or a proxy is trusted", () => {
    const named = req("http://web:3000/x", { host: "web:3000" });
    expect(hostAllowed(named, CONFIGURED)).toBe(true);
    expect(hostAllowed(named, TRUSTED)).toBe(true);
  });

  it("refuses a rebound page's same-origin GET and POST before anything else is considered", async () => {
    const headers = { host: "rebound.example:3000", origin: "http://rebound.example:3000", "sec-fetch-site": "same-origin" };
    for (const method of ["GET", "POST"]) {
      const res = refuseCrossOrigin(req("http://rebound.example:3000/api/wheel/v1/projects", headers, method));
      expect(res?.status).toBe(403);
      expect((await res!.json()).error.code).toBe("public_origin_required");
    }
  });
});

describe("the client's address", () => {
  it("is the rightmost X-Forwarded-For, the one the nearest proxy wrote", () => {
    expect(clientAddress(req("http://x/", { "x-forwarded-for": "6.6.6.6, 203.0.113.9" }))).toBe("203.0.113.9");
    expect(clientAddress(req("http://x/", {}))).toBeNull();
  });
});

const post: RequestOrigin = {
  method: "POST",
  origin: "https://wheel.example.com",
  secFetchSite: null,
  publicOrigin: "https://wheel.example.com",
};

describe("which requests may change state", () => {
  it("lets a page on the public origin post", () => {
    expect(isSameOrigin(post)).toBe(true);
    expect(isSameOrigin({ ...post, origin: "https://WHEEL.example.com" })).toBe(true);
  });

  it("accepts Sec-Fetch-Site: same-origin for a request that carries no Origin", () => {
    expect(isSameOrigin({ ...post, origin: null, secFetchSite: "same-origin" })).toBe(true);
  });

  it.each<[string, Partial<RequestOrigin>]>([
    ["another site's Origin", { origin: "https://evil.example" }],
    ["neither Origin nor Sec-Fetch-Site", { origin: null }],
    ["Origin: null, from a sandboxed frame", { origin: "null" }],
    ["an Origin that is not a URL", { origin: "not a url" }],
    ["the same host over plain http", { origin: "http://wheel.example.com" }],
    ["the same host on another port", { origin: "https://wheel.example.com:8443" }],
    ["a sibling subdomain", { origin: "https://evil.wheel.example.com", secFetchSite: "same-site" }],
    ["Sec-Fetch-Site: same-site with no Origin", { origin: null, secFetchSite: "same-site" }],
    ["Sec-Fetch-Site: cross-site, whatever Origin says", { secFetchSite: "cross-site" }],
    ["a mismatched Origin the browser still calls same-origin", { origin: "http://rebound.example", secFetchSite: "same-origin" }],
    ["a public origin nobody can compute", { origin: "file://", publicOrigin: "null" }],
  ])("refuses a POST with %s", (_label, patch) => {
    expect(isSameOrigin({ ...post, ...patch })).toBe(false);
  });

  it.each(["PUT", "PATCH", "DELETE", "post"])("holds %s to the same rule as POST", (method) => {
    expect(isSameOrigin({ ...post, method, origin: "https://evil.example" })).toBe(false);
    expect(isSameOrigin({ ...post, method })).toBe(true);
  });

  it.each(["GET", "HEAD", "OPTIONS", "get"])("lets a %s through without an Origin — it changes nothing", (method) => {
    expect(isSameOrigin({ ...post, method, origin: null })).toBe(true);
  });

  it("refuses even a GET the browser labels cross-site: nothing legitimate embeds these routes", () => {
    expect(isSameOrigin({ ...post, method: "GET", secFetchSite: "cross-site" })).toBe(false);
  });
});

describe("the three deployments, end to end", () => {
  it("direct http on localhost: the page's own POST passes, another site's does not", () => {
    const own = req("http://localhost:3000/api/session/login", { host: "localhost:3000", origin: "http://localhost:3000" });
    const hostile = req("http://localhost:3000/api/session/login", { host: "localhost:3000", origin: "http://evil.example" });
    expect(refuseCrossOrigin(own)).toBeNull();
    expect(refuseCrossOrigin(hostile)?.status).toBe(403);
  });

  it("behind a trusted proxy: Origin is compared with the public origin, not the internal host", () => {
    vi.stubEnv("WHEEL_TRUST_PROXY", "1");
    const behind = { host: "web:3000", "x-forwarded-proto": "https", "x-forwarded-host": "wheel.example.com" };
    const own = req("http://0.0.0.0:3000/api/session/login", { ...behind, origin: "https://wheel.example.com" });
    const internal = req("http://0.0.0.0:3000/api/session/login", { ...behind, origin: "http://web:3000" });
    expect(refuseCrossOrigin(own)).toBeNull();
    expect(refuseCrossOrigin(internal)?.status).toBe(403);
  });

  it("a forged X-Forwarded-* from an untrusted peer cannot make a foreign Origin match", () => {
    const forged = req("http://wheel.example.com:3000/api/session/login", {
      host: "wheel.example.com:3000",
      origin: "https://evil.example",
      "x-forwarded-proto": "https",
      "x-forwarded-host": "evil.example",
    });
    expect(isSameOrigin(requestOrigin(forged, NO_TRUST))).toBe(false);
    // …and claiming https for the real host buys nothing either: the connection was http.
    const upgraded = req("http://wheel.example.com:3000/api/session/login", {
      host: "wheel.example.com:3000",
      origin: "https://wheel.example.com:3000",
      "x-forwarded-proto": "https",
    });
    expect(isSameOrigin(requestOrigin(upgraded, NO_TRUST))).toBe(false);
  });
});

describe("refuseCrossOrigin", () => {
  beforeEach(() => vi.stubEnv("WHEEL_PUBLIC_ORIGIN", "https://wheel.example"));

  it("answers 403 in the API's own error shape", async () => {
    const res = refuseCrossOrigin(req("https://wheel.example/api/x", { origin: "https://evil.example", host: "wheel.example" }));
    expect(res?.status).toBe(403);
    expect(await res!.json()).toEqual({ error: { code: "cross_origin", message: expect.any(String) } });
  });

  it("lets a same-origin request carry on", () => {
    expect(refuseCrossOrigin(req("https://wheel.example/api/x", { origin: "https://wheel.example", host: "wheel.example" }))).toBeNull();
  });

  it("tells the operator to set WHEEL_PUBLIC_ORIGIN when the browser says same-origin but the origins disagree", () => {
    vi.stubEnv("WHEEL_PUBLIC_ORIGIN", "");
    vi.stubEnv("WHEEL_TRUST_PROXY", "1");
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    refuseCrossOrigin(
      req("http://0.0.0.0:3000/api/x", { host: "web:3000", origin: "https://wheel.example.com", "sec-fetch-site": "same-origin" }),
    );
    expect(warn).toHaveBeenCalledWith(expect.stringContaining("WHEEL_PUBLIC_ORIGIN"));
  });

  it("stays quiet about an ordinary cross-site attempt", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    refuseCrossOrigin(req("https://wheel.example/api/x", { host: "wheel.example", origin: "https://evil.example", "sec-fetch-site": "cross-site" }));
    expect(warn).not.toHaveBeenCalled();
  });
});
