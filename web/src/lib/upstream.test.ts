// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { expiredJwt, liveJwt } from "../../test/tokens";

const getToken = vi.hoisted(() => vi.fn());
vi.mock("@clerk/nextjs/server", () => ({ auth: async () => ({ getToken }) }));

import {
  MOCK_TOKEN,
  answered,
  apiFailed,
  apiUrl,
  callApi,
  clearCookieOn401,
  passThrough,
  unauthenticated,
  upstreamToken,
} from "./upstream";

const TOKEN = liveJwt({ sub: "u1" });
const withCookie = (cookie: string, headers: Record<string, string> = {}) =>
  new Request("http://localhost:3000/api/wheel/v1/projects", { headers: { cookie, ...headers } });

beforeEach(() => {
  vi.stubEnv("WHEEL_API_URL", "http://api.test:8080");
  vi.stubEnv("WHEEL_DEV_TOKEN", "");
  vi.stubEnv("WHEEL_PUBLIC_ORIGIN", "");
  vi.stubEnv("WHEEL_TRUST_PROXY", "");
  vi.stubEnv("VERCEL", "");
  getToken.mockReset();
});

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
});

describe("the credential this server presents", () => {
  it("in local mode is the session cookie, when it is a live JWT", async () => {
    expect(await upstreamToken(withCookie(`wheel_session=${TOKEN}`), "local")).toBe(TOKEN);
    expect(await upstreamToken(withCookie("other=1"), "local")).toBeNull();
  });

  it.each([
    ["any string at all", "tok"],
    ["an expired JWT", expiredJwt()],
    ["a token of the wrong shape", "a.b"],
  ])("in local mode is nothing for %s: no body is read and no socket dialled for it", async (_label, value) => {
    expect(await upstreamToken(withCookie(`wheel_session=${value}`), "local")).toBeNull();
  });

  it("never a credential the browser supplied itself", async () => {
    const req = withCookie("other=1", { "x-auth-token": TOKEN, authorization: `Bearer ${TOKEN}` });
    expect(await upstreamToken(req, "local")).toBeNull();
  });

  it("in clerk mode is Clerk's server-side token", async () => {
    getToken.mockResolvedValue("clerk.jwt");
    expect(await upstreamToken(withCookie(""), "clerk")).toBe("clerk.jwt");
    getToken.mockResolvedValue(null);
    expect(await upstreamToken(withCookie(""), "clerk")).toBeNull();
  });

  it("in dev mode is WHEEL_DEV_TOKEN, and nothing when it is unset", async () => {
    vi.stubEnv("WHEEL_DEV_TOKEN", "dev.jwt");
    expect(await upstreamToken(withCookie(""), "dev")).toBe("dev.jwt");
    vi.stubEnv("WHEEL_DEV_TOKEN", "");
    vi.stubEnv("NEXT_PUBLIC_DEV_TOKEN", "bundled");
    expect(await upstreamToken(withCookie(""), "dev")).toBeNull();
  });

  // Security review, finding 2: a real dev token must never ride the mock mode an unset
  // WHEEL_AUTH_MODE falls into.
  it("in mock mode is the mock's constant and nothing else, even with a real dev token set", async () => {
    vi.stubEnv("WHEEL_DEV_TOKEN", "secret-dev-token");
    expect(await upstreamToken(withCookie(""), "mock")).toBe(MOCK_TOKEN);
  });

  it("follows WHEEL_AUTH_MODE when no mode is passed", async () => {
    vi.stubEnv("WHEEL_AUTH_MODE", "dev");
    vi.stubEnv("WHEEL_DEV_TOKEN", "from-env");
    expect(await upstreamToken(withCookie(`wheel_session=${TOKEN}`))).toBe("from-env");
  });
});

describe("apiUrl", () => {
  it("builds on WHEEL_API_URL", () => {
    expect(apiUrl("/v1/auth/me")).toBe("http://api.test:8080/v1/auth/me");
  });

  it("refuses a path that would not stay where it was aimed", () => {
    expect(() => apiUrl("/v1/auth/../../admin")).toThrow(/refusing/);
  });
});

describe("callApi", () => {
  it("attaches the credential and project id here, sends JSON, and never follows a redirect", async () => {
    const fetchMock = vi.fn(async () => new Response("{}"));
    vi.stubGlobal("fetch", fetchMock);
    await callApi("http://api.test:8080/v1/x", { method: "POST", token: "tok", projectId: "p1", json: { a: 1 } });
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit & { duplex?: string }];
    const headers = new Headers(init.headers);
    expect(url).toBe("http://api.test:8080/v1/x");
    expect(headers.get("x-auth-token")).toBe("tok");
    expect(headers.get("x-project-id")).toBe("p1");
    expect(headers.get("content-type")).toBe("application/json");
    expect(init.body).toBe('{"a":1}');
    expect(init.redirect).toBe("manual");
    expect(init.cache).toBe("no-store");
    expect(init.duplex).toBeUndefined();
  });

  it("sends a stream body half-duplex, as Node requires", async () => {
    const fetchMock = vi.fn(async () => new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);
    const body = new ReadableStream<Uint8Array>();
    await callApi("http://api.test:8080/v1/x", { method: "PUT", body, headers: new Headers({ "content-type": "image/png" }) });
    const [, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit & { duplex?: string }];
    expect(init.body).toBe(body);
    expect(init.duplex).toBe("half");
    expect(new Headers(init.headers).get("content-type")).toBe("image/png");
    expect(new Headers(init.headers).has("x-auth-token")).toBe(false);
  });

  it("reports an API that cannot be reached at all", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => {
      throw new TypeError("fetch failed");
    }));
    expect(await callApi("http://api.test:8080/v1/x", { method: "GET" })).toEqual({ failure: "unreachable" });
  });

  it("gives up on an API that does not answer in time, and says it timed out", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        (_url: string, init: RequestInit) =>
          new Promise<Response>((_resolve, reject) => init.signal!.addEventListener("abort", () => reject(init.signal!.reason))),
      ),
    );
    const started = Date.now();
    expect(await callApi("http://api.test:8080/v1/x", { method: "GET", timeoutMs: 30 })).toEqual({ failure: "timeout" });
    expect(Date.now() - started).toBeLessThan(2_000);
  });

  it("still honours the caller's own abort alongside the deadline, as unreachable rather than timed out", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        (_url: string, init: RequestInit) =>
          new Promise<Response>((_resolve, reject) => init.signal!.addEventListener("abort", () => reject(init.signal!.reason))),
      ),
    );
    const caller = new AbortController();
    const pending = callApi("http://api.test:8080/v1/x", { method: "GET", signal: caller.signal, timeoutMs: 60_000 });
    caller.abort();
    expect(await pending).toEqual({ failure: "unreachable" });
  });

  it("tells an answer from a failure", () => {
    expect(answered(new Response(null))).toBe(true);
    expect(answered({ failure: "timeout" })).toBe(false);
  });
});

describe("handing the API's answer back", () => {
  it("keeps status, text and body exactly, with only the allowed headers", async () => {
    const upstream = new Response('{"error":{"code":"conflict","message":"taken"}}', {
      status: 409,
      statusText: "Conflict",
      headers: { "content-type": "application/json", "set-cookie": "x=1", "retry-after": "5" },
    });
    const res = passThrough(upstream);
    expect(res.status).toBe(409);
    expect(res.statusText).toBe("Conflict");
    expect(await res.text()).toBe('{"error":{"code":"conflict","message":"taken"}}');
    expect(res.headers.get("retry-after")).toBe("5");
    expect(res.headers.get("set-cookie")).toBeNull();
    expect(res.headers.get("x-content-type-options")).toBe("nosniff");
  });

  it("sends no body with a status that must not have one", async () => {
    const res = passThrough(new Response(null, { status: 204 }));
    expect(res.status).toBe(204);
    expect(res.body).toBeNull();
  });

  it("appends extra headers, such as a cleared cookie", () => {
    const res = passThrough(new Response("x", { status: 401 }), [["set-cookie", "wheel_session=; Max-Age=0"]]);
    expect(res.headers.get("set-cookie")).toBe("wheel_session=; Max-Age=0");
  });

  it.each<["unreachable" | "timeout", number, string]>([
    ["unreachable", 502, "api_unreachable"],
    ["timeout", 504, "api_timeout"],
  ])("reports %s as %i in the API's own shape", async (failure, status, code) => {
    const res = apiFailed({ failure });
    expect(res.status).toBe(status);
    expect((await res.json()).error.code).toBe(code);
  });
});

describe("a missing or dead session", () => {
  it("is a 401 that clears the cookie in local mode", async () => {
    const res = unauthenticated(new Request("https://wheel.example/x"), "local");
    expect(res.status).toBe(401);
    expect(res.headers.get("set-cookie")).toMatch(/^__Host-wheel_session=; .*Max-Age=0/);
    expect((await res.json()).error.code).toBe("unauthenticated");
  });

  it("names the server-only variable to set in dev mode, and touches no cookie", async () => {
    const res = unauthenticated(new Request("http://localhost/x"), "dev");
    expect(res.headers.get("set-cookie")).toBeNull();
    expect((await res.json()).error.message).toContain("WHEEL_DEV_TOKEN");
  });

  it.each<[number, "local" | "clerk", number]>([
    [401, "local", 1],
    [403, "local", 0],
    [401, "clerk", 0],
  ])("clears the cookie on %i in %s mode: %i header(s)", (status, mode, count) => {
    expect(clearCookieOn401(status, new Request("http://localhost/"), mode)).toHaveLength(count);
  });
});
