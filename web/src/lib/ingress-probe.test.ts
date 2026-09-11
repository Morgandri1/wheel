// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { liveJwt } from "../../test/tokens";
import { API_CALL_TIMEOUT_MS } from "./upstream";
import { HIT_TIMEOUT_MS, ingressPath, probeIngress } from "./ingress-probe";

const API = "http://api.test:8080";
const TOKEN = liveJwt({ sub: "u1" });
const fetchMock = vi.fn<(input: string, init: RequestInit) => Promise<Response>>();

beforeEach(() => {
  vi.stubEnv("WHEEL_API_URL", API);
  vi.stubEnv("WHEEL_AUTH_MODE", "local");
  vi.stubEnv("WHEEL_PUBLIC_ORIGIN", "http://wheel.test");
  vi.stubEnv("WHEEL_TRUST_PROXY", "");
  vi.stubEnv("VERCEL", "");
  fetchMock.mockReset().mockImplementation(async (url) =>
    url.includes("/p/")
      ? Response.json({ queued: 1 }, { status: 202, statusText: "Accepted" })
      : Response.json({ id: "p1" }),
  );
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
  // Belt for the AbortSignal.timeout spies below: if one of those tests threw before reaching its
  // own mockRestore(), a broken spy left in place recurses into every later test that dials out.
  vi.restoreAllMocks();
});

function probe(body: unknown, init: { cookie?: string | null; origin?: string; contentType?: string } = {}) {
  const headers: Record<string, string> = {
    host: "wheel.test",
    origin: init.origin ?? "http://wheel.test",
    "content-type": init.contentType ?? "application/json",
  };
  if (init.cookie !== null) headers.cookie = init.cookie ?? `wheel_session=${TOKEN}`;
  return probeIngress(
    new Request("http://wheel.test/api/wheel/probe", { method: "POST", headers, body: JSON.stringify(body) }),
  );
}

const sent = (i: number) => {
  const [url, init] = fetchMock.mock.calls[i]!;
  return { url, init, headers: new Headers(init.headers) };
};

describe("the endpoint path", () => {
  it.each<[unknown, string | null]>([
    ["/hook", "/hook"],
    ["/", "/"],
    ["/a/b c", "/a/b%20c"],
    ["hook", null],
    ["/..", null],
    ["/a/../../v1/auth/login", null],
    ["/./x", null],
    ["/a\\b", null],
    ["/a\u0000", null],
    ["/a\u001f", null],
    // A literal % would be a second layer of encoding once this encodes it.
    ["/a%2Fb", null],
    ["/%2e%2e/%2e%2e/v1/auth", null],
    ["/100%", null],
    [`/${"x".repeat(1100)}`, null],
    [42, null],
  ])("reads %j as %j", (path, expected) => {
    expect(ingressPath(path)).toBe(expected);
  });
});

describe("probing", () => {
  it("proves ownership with the caller's session, then hits the ingress carrying no credential at all", async () => {
    const res = await probe({ project_id: "p1", method: "POST", path: "/hook" });

    expect(sent(0).url).toBe(`${API}/v1/projects/p1`);
    expect(sent(0).headers.get("x-auth-token")).toBe(TOKEN);
    expect(sent(0).init.signal).toBeInstanceOf(AbortSignal);

    expect(sent(1).url).toBe(`${API}/p/p1/hook`);
    expect(sent(1).init.method).toBe("POST");
    expect(sent(1).headers.has("x-auth-token")).toBe(false);
    expect(sent(1).headers.has("cookie")).toBe(false);
    expect(sent(1).init.body).toBe('{"source":"wheel-endpoint-test"}');
    expect(sent(1).init.signal).toBeInstanceOf(AbortSignal);

    expect(await res.json()).toEqual({ status: 202, status_text: "Accepted", body: '{"queued":1}', truncated: false });
  });

  it("sends no body with a GET", async () => {
    await probe({ project_id: "p1", method: "GET", path: "/hook" });
    expect(sent(1).init.method).toBe("GET");
    expect(sent(1).init.body).toBeUndefined();
  });

  it("reports what the ingress said even when it is an error, and truncates a long answer", async () => {
    fetchMock.mockImplementation(async (url) =>
      url.includes("/p/") ? new Response("x".repeat(70_000), { status: 404 }) : Response.json({}),
    );
    const reading = await (await probe({ project_id: "p1", method: "GET", path: "/hook" })).json();
    expect(reading.status).toBe(404);
    expect(reading.truncated).toBe(true);
    expect(reading.body.length).toBe(64 * 1024);
  });

  it("never reaches the ingress for a project the caller does not own", async () => {
    fetchMock.mockResolvedValue(Response.json({ error: { code: "not_found", message: "no" } }, { status: 404 }));
    const res = await probe({ project_id: "p2", method: "GET", path: "/hook" });
    expect(res.status).toBe(404);
    expect(fetchMock).toHaveBeenCalledOnce();
  });

  it("clears the cookie when the ownership check finds the session dead", async () => {
    fetchMock.mockResolvedValue(new Response("{}", { status: 401 }));
    const res = await probe({ project_id: "p1", method: "GET", path: "/hook" });
    expect(res.status).toBe(401);
    expect(res.headers.get("set-cookie")).toMatch(/Max-Age=0/);
  });

  it("answers 401 without a session, and calls nothing", async () => {
    const res = await probe({ project_id: "p1", method: "GET", path: "/hook" }, { cookie: null });
    expect(res.status).toBe(401);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it.each([
    ["an unsupported method", { project_id: "p1", method: "PATCH", path: "/hook" }],
    ["a project id with a path in it", { project_id: "../v1", method: "GET", path: "/hook" }],
    ["a path that climbs out", { project_id: "p1", method: "GET", path: "/../../v1/auth/me" }],
    ["a pre-encoded path", { project_id: "p1", method: "GET", path: "/%252e%252e/v1" }],
    ["no path", { project_id: "p1", method: "GET" }],
    ["not an object", "probe"],
  ])("refuses %s before calling anything", async (_label, body) => {
    const res = await probe(body);
    expect(res.status).toBe(400);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("refuses a body that is not declared JSON", async () => {
    const res = await probe({ project_id: "p1", method: "GET", path: "/hook" }, { contentType: "text/plain" });
    expect(res.status).toBe(415);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("refuses a cross-origin request", async () => {
    const res = await probe({ project_id: "p1", method: "GET", path: "/hook" }, { origin: "https://evil.example" });
    expect(res.status).toBe(403);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("reports an unreachable API on either call", async () => {
    fetchMock.mockRejectedValueOnce(new TypeError("fetch failed"));
    expect((await probe({ project_id: "p1", method: "GET", path: "/hook" })).status).toBe(502);

    fetchMock.mockReset().mockImplementation(async (url) => {
      if (url.includes("/p/")) throw new TypeError("fetch failed");
      return Response.json({});
    });
    expect((await probe({ project_id: "p1", method: "GET", path: "/hook" })).status).toBe(502);
  });
});

/**
 * QA review round 2: the hit was bound to the same 5s deadline as the ownership check, so a
 * `script` endpoint doing real work (default timeout 60s, ceiling 300s) got misreported as
 * "the test did not run" the moment it ran past 5s. The ownership check is a small, fast lookup
 * and keeps its 5s deadline; the hit gets its own, longer one, and a hit that outlives IT is
 * reported as delivered, not failed.
 */
describe("the ownership check and the hit have different deadlines", () => {
  it("asks AbortSignal.timeout for 5s on the ownership check and 30s on the hit — never the same value", async () => {
    const requested: number[] = [];
    const realTimeout = AbortSignal.timeout.bind(AbortSignal);
    const spy = vi.spyOn(AbortSignal, "timeout").mockImplementation((ms: number) => {
      requested.push(ms);
      // Fire almost immediately regardless of the real deadline, so this test costs milliseconds.
      return realTimeout(5);
    });
    fetchMock.mockImplementation((url: string, init: RequestInit) =>
      url.includes("/p/")
        ? new Promise<Response>((_resolve, reject) => init.signal!.addEventListener("abort", () => reject(init.signal!.reason)))
        : Promise.resolve(Response.json({ id: "p1" })),
    );

    await probe({ project_id: "p1", method: "GET", path: "/hook" });

    expect(requested).toEqual([API_CALL_TIMEOUT_MS, HIT_TIMEOUT_MS]);
    expect(HIT_TIMEOUT_MS).toBeGreaterThan(API_CALL_TIMEOUT_MS);
    spy.mockRestore();
  });

  it("reports a hit that outlives its own deadline as SENT, not as a failure", async () => {
    const realTimeout = AbortSignal.timeout.bind(AbortSignal);
    const spy = vi.spyOn(AbortSignal, "timeout").mockImplementation((ms: number) => realTimeout(Math.min(ms, 5)));
    fetchMock.mockImplementation((url: string, init: RequestInit) =>
      url.includes("/p/")
        ? new Promise<Response>((_resolve, reject) => init.signal!.addEventListener("abort", () => reject(init.signal!.reason)))
        : Promise.resolve(Response.json({ id: "p1" })),
    );

    const res = await probe({ project_id: "p1", method: "GET", path: "/hook" });

    // A 200 the panel reads as "delivered", never the 502/504 apiFailed would answer.
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ sent: true, timeout_ms: HIT_TIMEOUT_MS });
    spy.mockRestore();
  });

  it("still reports the API genuinely being unreachable as a failure, not as sent", async () => {
    fetchMock.mockImplementation((url: string) =>
      url.includes("/p/") ? Promise.reject(new TypeError("fetch failed")) : Promise.resolve(Response.json({ id: "p1" })),
    );

    const res = await probe({ project_id: "p1", method: "GET", path: "/hook" });
    expect(res.status).toBe(502);
    expect((await res.json()).error.code).toBe("api_unreachable");
  });
});
