// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { expiredJwt, liveJwt } from "../../test/tokens";
import { acceptInvite } from "./invite-routes";

const API = "http://api.test:8080";
const TOKEN = liveJwt({ sub: "u1" });
const fetchMock = vi.fn<(input: string, init: RequestInit) => Promise<Response>>();

beforeEach(() => {
  vi.stubEnv("WHEEL_API_URL", API);
  vi.stubEnv("WHEEL_AUTH_MODE", "local");
  vi.stubEnv("WHEEL_PUBLIC_ORIGIN", "http://wheel.test");
  vi.stubEnv("WHEEL_TRUST_PROXY", "");
  vi.stubEnv("VERCEL", "");
  fetchMock.mockReset();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
});

function post(body: unknown, init: { cookie?: string; contentType?: string | null } = {}) {
  const base = "http://wheel.test";
  const headers: Record<string, string> = { host: "wheel.test", origin: base };
  if (init.contentType !== null) headers["content-type"] = init.contentType ?? "application/json";
  if (init.cookie) headers.cookie = init.cookie;
  return new Request(`${base}/api/invites/accept`, {
    method: "POST",
    headers,
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
}

function sent(i = 0) {
  const [url, init] = fetchMock.mock.calls[i]!;
  return { url, init, headers: new Headers(init.headers), json: init.body ? JSON.parse(init.body as string) : undefined };
}

describe("redeeming an invite", () => {
  it("forwards the token with the caller's session, nothing else", async () => {
    fetchMock.mockResolvedValue(Response.json({ project_id: "p1", role: "prompter" }));
    const res = await acceptInvite(post({ token: "wi_abc123" }, { cookie: `wheel_session=${TOKEN}` }));
    expect(sent().url).toBe(`${API}/v1/invites/accept`);
    expect(sent().headers.get("x-auth-token")).toBe(TOKEN);
    expect(sent().json).toEqual({ token: "wi_abc123" });
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ project_id: "p1", role: "prompter" });
  });

  it.each([
    ["no session", undefined],
    ["an expired session", `wheel_session=${expiredJwt()}`],
  ])("answers 401 with %s, before calling anything", async (_label, cookie) => {
    const res = await acceptInvite(post({ token: "wi_abc123" }, { cookie }));
    expect(res.status).toBe(401);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("refuses a body with no token", async () => {
    const res = await acceptInvite(post({}, { cookie: `wheel_session=${TOKEN}` }));
    expect(res.status).toBe(400);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("refuses a blank token without asking the API", async () => {
    const res = await acceptInvite(post({ token: "   " }, { cookie: `wheel_session=${TOKEN}` }));
    expect(res.status).toBe(400);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("refuses a non-JSON content type", async () => {
    const res = await acceptInvite(
      post("token=wi_abc", { cookie: `wheel_session=${TOKEN}`, contentType: "text/plain" }),
    );
    expect(res.status).toBe(415);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  /**
   * The API answers an unusable invite with the SAME generic 401 it uses for a dead session. This
   * route used to treat every 401 as a dead session, clear the cookie, and sign out a visitor whose
   * only mistake was a stale link (found by QA's multiplayer e2e, S2). Only the session itself can
   * say which it was, so the route asks `GET /v1/auth/me`.
   */
  describe("when the API answers the invite with a 401", () => {
    const generic = () =>
      Response.json({ error: { code: "unauthorized", message: "Missing or invalid authentication token." } }, { status: 401 });
    const bySession = (meStatus: number) =>
      fetchMock.mockImplementation(async (url) => (String(url).endsWith("/v1/auth/me") ? new Response("{}", { status: meStatus }) : generic()));
    const dead = () => acceptInvite(post({ token: "wi_dead" }, { cookie: `wheel_session=${TOKEN}` }));

    it("keeps a LIVE session: the visitor loses the link, not their login", async () => {
      bySession(200);
      const res = await dead();
      expect(res.headers.get("set-cookie")).toBeNull();
      expect(res.status).toBe(403);
      const body = await res.json();
      expect(body.error.code).toBe("invite_unusable");
      expect(body.error.message).toMatch(/invite link can't be used/);
      expect(body.error.message).not.toMatch(/authentication token/);
    });

    it("still clears the cookie when the session ITSELF is dead (revoked server-side)", async () => {
      bySession(401);
      const res = await dead();
      expect(res.status).toBe(401);
      expect(res.headers.get("set-cookie")).toMatch(/Max-Age=0/);
    });

    it("asks with the caller's own session, and only after the invite call", async () => {
      bySession(200);
      await dead();
      expect(fetchMock).toHaveBeenCalledTimes(2);
      expect(sent(0).url).toBe(`${API}/v1/invites/accept`);
      expect(sent(1).url).toBe(`${API}/v1/auth/me`);
      expect(sent(1).headers.get("x-auth-token")).toBe(TOKEN);
    });

    it("does not sign anyone out on uncertainty: a probe that cannot be answered leaves the cookie", async () => {
      fetchMock.mockImplementation(async (url) => {
        if (String(url).endsWith("/v1/auth/me")) throw new TypeError("fetch failed");
        return generic();
      });
      const res = await dead();
      expect(res.status).toBe(403);
      expect(res.headers.get("set-cookie")).toBeNull();
    });

    it("makes no probe outside local mode: there is no cookie to lose and no /v1/auth/me to ask", async () => {
      vi.stubEnv("WHEEL_AUTH_MODE", "dev");
      vi.stubEnv("WHEEL_DEV_TOKEN", "dev-token");
      fetchMock.mockImplementation(async () => generic());
      const res = await acceptInvite(post({ token: "wi_dead" }));
      expect(fetchMock).toHaveBeenCalledTimes(1);
      expect(res.status).toBe(403);
      expect((await res.json()).error.code).toBe("invite_unusable");
    });
  });

  it.each([
    [400, "bad_request"],
    [409, "conflict"],
    [500, "internal"],
  ])("passes a %s through unchanged and never probes the session", async (status, code) => {
    fetchMock.mockResolvedValue(Response.json({ error: { code, message: "engine words" } }, { status }));
    const res = await acceptInvite(post({ token: "wi_abc" }, { cookie: `wheel_session=${TOKEN}` }));
    expect(res.status).toBe(status);
    expect((await res.json()).error.message).toBe("engine words");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("reports an unreachable API", async () => {
    fetchMock.mockRejectedValue(new TypeError("fetch failed"));
    const res = await acceptInvite(post({ token: "wi_abc123" }, { cookie: `wheel_session=${TOKEN}` }));
    expect(res.status).toBe(502);
  });
});
