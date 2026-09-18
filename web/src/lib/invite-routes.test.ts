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

  it("passes an unusable invite's answer through unchanged — the API's wording, not ours", async () => {
    fetchMock.mockResolvedValue(
      Response.json({ error: { code: "unauthorized", message: "Missing or invalid authentication token." } }, { status: 401 }),
    );
    const res = await acceptInvite(post({ token: "wi_dead" }, { cookie: `wheel_session=${TOKEN}` }));
    expect(res.status).toBe(401);
    expect((await res.json()).error.message).toBe("Missing or invalid authentication token.");
  });

  it("clears this browser's own session cookie when the API says it, specifically, is dead", async () => {
    fetchMock.mockResolvedValue(new Response("{}", { status: 401 }));
    const res = await acceptInvite(post({ token: "wi_dead" }, { cookie: `wheel_session=${TOKEN}` }));
    expect(res.headers.get("set-cookie")).toMatch(/Max-Age=0/);
  });

  it("reports an unreachable API", async () => {
    fetchMock.mockRejectedValue(new TypeError("fetch failed"));
    const res = await acceptInvite(post({ token: "wi_abc123" }, { cookie: `wheel_session=${TOKEN}` }));
    expect(res.status).toBe(502);
  });
});
