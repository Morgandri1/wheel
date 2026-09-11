// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const getToken = vi.hoisted(() => vi.fn());
vi.mock("@clerk/nextjs/server", () => ({ auth: async () => ({ getToken }) }));

import {
  MOCK_TOKEN,
  apiUnreachable,
  apiUrl,
  callApi,
  clearCookieOn401,
  passThrough,
  unauthenticated,
  upstreamToken,
} from "./upstream";

const withCookie = (cookie: string, headers: Record<string, string> = {}) =>
  new Request("http://wheel.test/api/wheel/v1/projects", { headers: { cookie, ...headers } });

beforeEach(() => {
  vi.stubEnv("WHEEL_API_URL", "http://api.test:8080");
  vi.stubEnv("WHEEL_DEV_TOKEN", "");
  vi.stubEnv("NEXT_PUBLIC_DEV_TOKEN", "");
  getToken.mockReset();
});

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
});

describe("the credential this server presents", () => {
  it("in local mode is the session cookie", async () => {
    expect(await upstreamToken(withCookie("wheel_session=tok"), "local")).toBe("tok");
    expect(await upstreamToken(withCookie("other=1"), "local")).toBeNull();
  });

  it("never a credential the browser supplied itself", async () => {
    const req = withCookie("other=1", { "x-auth-token": "attacker", authorization: "Bearer attacker" });
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

  it("in mock mode is the mock's constant unless a dev token is set", async () => {
    expect(await upstreamToken(withCookie(""), "mock")).toBe(MOCK_TOKEN);
    vi.stubEnv("WHEEL_DEV_TOKEN", "override");
    expect(await upstreamToken(withCookie(""), "mock")).toBe("override");
  });

  it("follows WHEEL_AUTH_MODE when no mode is passed", async () => {
    vi.stubEnv("WHEEL_AUTH_MODE", "dev");
    vi.stubEnv("WHEEL_DEV_TOKEN", "from-env");
    expect(await upstreamToken(withCookie("wheel_session=cookie"))).toBe("from-env");
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
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    const headers = new Headers(init.headers);
    expect(url).toBe("http://api.test:8080/v1/x");
    expect(headers.get("x-auth-token")).toBe("tok");
    expect(headers.get("x-project-id")).toBe("p1");
    expect(headers.get("content-type")).toBe("application/json");
    expect(init.body).toBe('{"a":1}');
    expect(init.redirect).toBe("manual");
    expect(init.cache).toBe("no-store");
  });

  it("passes a raw body and the given headers through", async () => {
    const fetchMock = vi.fn(async () => new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);
    const body = new Uint8Array([1, 2, 3]);
    await callApi("http://api.test:8080/v1/x", { method: "PUT", body, headers: new Headers({ "content-type": "image/png" }) });
    const [, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(init.body).toBe(body);
    expect(new Headers(init.headers).get("content-type")).toBe("image/png");
    expect(new Headers(init.headers).has("x-auth-token")).toBe(false);
  });

  it("is null when the API cannot be reached at all", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => {
      throw new TypeError("fetch failed");
    }));
    expect(await callApi("http://api.test:8080/v1/x", { method: "GET" })).toBeNull();
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

  it("reports an unreachable API as a 502 in the API's own shape", async () => {
    const res = apiUnreachable();
    expect(res.status).toBe(502);
    expect(await res.json()).toEqual({ error: { code: "api_unreachable", message: expect.stringMatching(/can't reach the api/i) } });
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
    const res = unauthenticated(new Request("http://wheel.test/x"), "dev");
    expect(res.headers.get("set-cookie")).toBeNull();
    expect((await res.json()).error.message).toContain("WHEEL_DEV_TOKEN");
  });

  it.each<[number, "local" | "clerk", number]>([
    [401, "local", 1],
    [403, "local", 0],
    [401, "clerk", 0],
  ])("clears the cookie on %i in %s mode: %i header(s)", (status, mode, count) => {
    expect(clearCookieOn401(status, new Request("http://wheel.test/"), mode)).toHaveLength(count);
  });
});
