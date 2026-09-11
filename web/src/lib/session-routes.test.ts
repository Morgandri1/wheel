// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { getSession, sessionAction } from "./session-routes";

const API = "http://api.test:8080";
const USER = { id: "u1", email: "dev@wheel.dev" };
const fetchMock = vi.fn<(input: string, init: RequestInit) => Promise<Response>>();

beforeEach(() => {
  vi.stubEnv("WHEEL_API_URL", API);
  vi.stubEnv("WHEEL_AUTH_MODE", "local");
  fetchMock.mockReset();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
});

function post(path: string, body: unknown, init: { cookie?: string; origin?: string; base?: string } = {}) {
  const base = init.base ?? "http://wheel.test";
  const headers: Record<string, string> = {
    host: new URL(base).host,
    origin: init.origin ?? base,
    "content-type": "application/json",
  };
  if (init.cookie) headers.cookie = init.cookie;
  return new Request(`${base}${path}`, {
    method: "POST",
    headers,
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
}

const get = (cookie?: string) =>
  new Request("http://wheel.test/api/session", { headers: cookie ? { cookie } : {} });

function sent(i = 0) {
  const [url, init] = fetchMock.mock.calls[i]!;
  return { url, init, headers: new Headers(init.headers), json: init.body ? JSON.parse(init.body as string) : undefined };
}

function cookieOf(res: Response) {
  return res.headers.get("set-cookie") ?? "";
}

describe("signing in", () => {
  beforeEach(() => {
    fetchMock.mockResolvedValue(
      Response.json({ token: "hdr.payload.sig", expires_at: new Date(Date.now() + 3600_000).toISOString(), user: USER }),
    );
  });

  it("forwards only the email and password, sets the httpOnly cookie, and answers with the user alone", async () => {
    const res = await sessionAction(post("/api/session/login", { email: "dev@wheel.dev", password: "pw", admin: true }), "login");
    expect(sent().url).toBe(`${API}/v1/auth/login`);
    expect(sent().json).toEqual({ email: "dev@wheel.dev", password: "pw" });
    expect(sent().headers.has("x-auth-token")).toBe(false);

    expect(res.status).toBe(200);
    const body = await res.text();
    expect(JSON.parse(body)).toEqual({ user: USER });
    // The token is in the cookie and nowhere page script can read it.
    expect(body).not.toContain("hdr.payload.sig");
    expect(res.headers.get("cache-control")).toBe("no-store");

    const cookie = cookieOf(res);
    expect(cookie).toMatch(/^wheel_session=hdr\.payload\.sig; /);
    expect(cookie).toContain("HttpOnly");
    expect(cookie).toContain("SameSite=Lax");
    expect(cookie).toContain("Path=/");
    expect(Number(/Max-Age=(\d+)/.exec(cookie)?.[1])).toBeGreaterThan(3590);
    expect(cookie).not.toContain("Secure");
  });

  it("marks the cookie Secure under __Host- when the page is https", async () => {
    const res = await sessionAction(
      post("/api/session/login", { email: "a@b.co", password: "pw" }, { base: "https://wheel.example" }),
      "login",
    );
    expect(cookieOf(res)).toMatch(/^__Host-wheel_session=hdr\.payload\.sig; .*Secure/);
  });

  it("keeps the API's 201 on sign-up", async () => {
    fetchMock.mockResolvedValue(Response.json({ token: "t.o.k", user: USER }, { status: 201 }));
    const res = await sessionAction(post("/api/session/signup", { email: "a@b.co", password: "long-enough-pw" }), "signup");
    expect(sent().url).toBe(`${API}/v1/auth/signup`);
    expect(res.status).toBe(201);
    // No expires_at and not a JWT: a browser-session cookie rather than a guessed lifetime.
    expect(cookieOf(res)).not.toContain("Max-Age");
  });

  it("passes a rejection through byte for byte, and sets nothing", async () => {
    const envelope = '{"error":{"code":"unauthorized","message":"login failed"}}';
    fetchMock.mockResolvedValue(new Response(envelope, { status: 401, headers: { "content-type": "application/json" } }));
    const res = await sessionAction(post("/api/session/login", { email: "a@b.co", password: "x" }), "login");
    expect(res.status).toBe(401);
    expect(await res.text()).toBe(envelope);
    expect(res.headers.get("set-cookie")).toBeNull();
  });

  it("passes a lockout's retry-after through, so the form can count down", async () => {
    fetchMock.mockResolvedValue(new Response("{}", { status: 429, headers: { "retry-after": "30" } }));
    const res = await sessionAction(post("/api/session/login", { email: "a@b.co", password: "x" }), "login");
    expect(res.status).toBe(429);
    expect(res.headers.get("retry-after")).toBe("30");
  });

  it.each([
    ["no token", { user: USER }],
    ["no user", { token: "t" }],
    ["a user without an email", { token: "t", user: { id: "u1" } }],
    ["a token that would break the header", { token: "a;b\r\nSet-Cookie: x=1", user: USER }],
    ["not JSON at all", "<html>"],
  ])("refuses an answer with %s rather than store a broken session", async (_label, payload) => {
    fetchMock.mockResolvedValue(new Response(typeof payload === "string" ? payload : JSON.stringify(payload)));
    const res = await sessionAction(post("/api/session/login", { email: "a@b.co", password: "x" }), "login");
    expect(res.status).toBe(502);
    expect((await res.json()).error.code).toBe("bad_auth_response");
    expect(res.headers.get("set-cookie")).toBeNull();
  });

  it("refuses a session that is already over rather than set a cookie that dies on arrival", async () => {
    fetchMock.mockResolvedValue(Response.json({ token: "t", expires_at: "2000-01-01T00:00:00Z", user: USER }));
    const res = await sessionAction(post("/api/session/login", { email: "a@b.co", password: "x" }), "login");
    expect(res.status).toBe(502);
    expect((await res.json()).error.message).toMatch(/clock/);
    expect(res.headers.get("set-cookie")).toBeNull();
  });

  it("says the API is unreachable when it is", async () => {
    fetchMock.mockRejectedValue(new TypeError("fetch failed"));
    const res = await sessionAction(post("/api/session/login", { email: "a@b.co", password: "x" }), "login");
    expect(res.status).toBe(502);
    expect((await res.json()).error.code).toBe("api_unreachable");
  });

  it.each([
    ["not JSON", "email=a&password=b"],
    ["a missing password", { email: "a@b.co" }],
    ["a number for an email", { email: 7, password: "x" }],
    ["an array", [1, 2]],
    ["null", "null"],
    ["an oversized body", { email: "a@b.co", password: "x".repeat(20_000) }],
  ])("refuses %s before calling the API", async (_label, body) => {
    const res = await sessionAction(post("/api/session/login", body), "login");
    expect(res.status).toBe(400);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe("what every action refuses", () => {
  it.each(["login", "signup", "logout", "password"])("a cross-origin %s", async (action) => {
    const res = await sessionAction(
      post(`/api/session/${action}`, { email: "a@b.co", password: "x" }, { origin: "https://evil.example", cookie: "wheel_session=t" }),
      action,
    );
    expect(res.status).toBe(403);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("any action outside local mode", async () => {
    vi.stubEnv("WHEEL_AUTH_MODE", "clerk");
    const res = await sessionAction(post("/api/session/login", { email: "a@b.co", password: "x" }), "login");
    expect(res.status).toBe(404);
    expect((await res.json()).error.code).toBe("not_local");
  });

  it("an action that does not exist", async () => {
    const res = await sessionAction(post("/api/session/me", {}), "me");
    expect(res.status).toBe(404);
  });
});

describe("signing out", () => {
  it("revokes the session at the API and clears the cookie", async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 204 }));
    const res = await sessionAction(post("/api/session/logout", {}, { cookie: "wheel_session=tok" }), "logout");
    expect(sent().url).toBe(`${API}/v1/auth/logout`);
    expect(sent().headers.get("x-auth-token")).toBe("tok");
    expect(res.status).toBe(204);
    expect(cookieOf(res)).toMatch(/^wheel_session=; .*Max-Age=0/);
  });

  it("clears the cookie even when the API cannot be reached", async () => {
    fetchMock.mockRejectedValue(new TypeError("fetch failed"));
    const res = await sessionAction(post("/api/session/logout", {}, { cookie: "wheel_session=tok" }), "logout");
    expect(res.status).toBe(204);
    expect(cookieOf(res)).toMatch(/Max-Age=0/);
  });

  it("does not call the API with no session to revoke", async () => {
    const res = await sessionAction(post("/api/session/logout", {}), "logout");
    expect(res.status).toBe(204);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe("changing the password", () => {
  const change = (cookie?: string, body: unknown = { current_password: "old-pw", new_password: "new-long-pw" }) =>
    sessionAction(post("/api/session/password", body, { cookie }), "password");

  it("forwards both passwords with the session, then clears the cookie, because the API revoked it", async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 204 }));
    const res = await change("wheel_session=tok");
    expect(sent().url).toBe(`${API}/v1/auth/password`);
    expect(sent().headers.get("x-auth-token")).toBe("tok");
    expect(sent().json).toEqual({ current_password: "old-pw", new_password: "new-long-pw" });
    expect(res.status).toBe(204);
    expect(cookieOf(res)).toMatch(/Max-Age=0/);
  });

  it("keeps the session when the API refuses the new password", async () => {
    fetchMock.mockResolvedValue(Response.json({ error: { code: "weak_password", message: "too short" } }, { status: 400 }));
    const res = await change("wheel_session=tok");
    expect(res.status).toBe(400);
    expect((await res.json()).error.message).toBe("too short");
    expect(res.headers.get("set-cookie")).toBeNull();
  });

  it("clears the cookie when the API says the session is dead", async () => {
    fetchMock.mockResolvedValue(new Response("{}", { status: 401 }));
    const res = await change("wheel_session=tok");
    expect(res.status).toBe(401);
    expect(cookieOf(res)).toMatch(/Max-Age=0/);
  });

  it("answers 401 without a session, before calling anything", async () => {
    const res = await change(undefined);
    expect(res.status).toBe(401);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("refuses a body without both passwords", async () => {
    const res = await change("wheel_session=tok", { new_password: "x" });
    expect(res.status).toBe(400);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("reports an unreachable API", async () => {
    fetchMock.mockRejectedValue(new TypeError("fetch failed"));
    expect((await change("wheel_session=tok")).status).toBe(502);
  });
});

describe("GET /api/session", () => {
  it("is no user, without a round trip, when there is no cookie", async () => {
    const res = await getSession(get());
    expect(await res.json()).toEqual({ user: null });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("asks the API who the cookie belongs to and answers with id and email only", async () => {
    fetchMock.mockResolvedValue(Response.json({ ...USER, created_at: "2026-01-01T00:00:00Z" }));
    const res = await getSession(get("wheel_session=tok"));
    expect(sent().url).toBe(`${API}/v1/auth/me`);
    expect(sent().headers.get("x-auth-token")).toBe("tok");
    expect(await res.json()).toEqual({ user: USER });
    expect(res.headers.get("cache-control")).toBe("no-store");
  });

  it("clears a dead cookie and answers no user", async () => {
    fetchMock.mockResolvedValue(new Response("{}", { status: 401 }));
    const res = await getSession(get("wheel_session=tok"));
    expect(await res.json()).toEqual({ user: null });
    expect(cookieOf(res)).toMatch(/Max-Age=0/);
  });

  it("passes any other failure through", async () => {
    fetchMock.mockResolvedValue(new Response('{"error":{"code":"boom","message":"db down"}}', { status: 500 }));
    const res = await getSession(get("wheel_session=tok"));
    expect(res.status).toBe(500);
    expect((await res.json()).error.code).toBe("boom");
  });

  it("reports an unreachable API", async () => {
    fetchMock.mockRejectedValue(new TypeError("fetch failed"));
    expect((await getSession(get("wheel_session=tok"))).status).toBe(502);
  });

  it("refuses an answer it cannot read", async () => {
    fetchMock.mockResolvedValue(Response.json({ id: 7 }));
    expect((await getSession(get("wheel_session=tok"))).status).toBe(502);
  });

  it("is no user outside local mode, whatever cookie is present", async () => {
    vi.stubEnv("WHEEL_AUTH_MODE", "mock");
    expect(await (await getSession(get("wheel_session=tok"))).json()).toEqual({ user: null });
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
