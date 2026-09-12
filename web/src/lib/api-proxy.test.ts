// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { expiredJwt, liveJwt } from "../../test/tokens";
import { proxyToApi } from "./api-proxy";

const API = "http://api.test:8080";
const TOKEN = liveJwt({ sub: "u1" });
const fetchMock = vi.fn<(input: string, init: RequestInit & { duplex?: string }) => Promise<Response>>();
let bodies: (Buffer | undefined)[] = [];

beforeEach(() => {
  vi.stubEnv("WHEEL_API_URL", API);
  vi.stubEnv("WHEEL_AUTH_MODE", "local");
  vi.stubEnv("WHEEL_PUBLIC_ORIGIN", "http://wheel.test");
  vi.stubEnv("WHEEL_TRUST_PROXY", "");
  vi.stubEnv("VERCEL", "");
  vi.stubEnv("WHEEL_PROXY_BODY_LIMIT_BYTES", "");
  bodies = [];
  // Reads the body the way a real upstream would, so a streamed body is actually pulled.
  fetchMock.mockReset().mockImplementation(async (_url, init) => {
    bodies.push(init.body ? Buffer.from(await new Response(init.body).arrayBuffer()) : undefined);
    return Response.json([{ id: "p1" }]);
  });
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
});

function browser(
  path: string,
  init: { method?: string; body?: BodyInit; headers?: Record<string, string>; origin?: string } = {},
) {
  return new Request(`${init.origin ?? "http://wheel.test"}${path}`, {
    method: init.method ?? "GET",
    body: init.body,
    duplex: init.body instanceof ReadableStream ? "half" : undefined,
    headers: {
      host: "wheel.test",
      origin: "http://wheel.test",
      "sec-fetch-site": "same-origin",
      cookie: `wheel_session=${TOKEN}; theme=dark`,
      ...init.headers,
    },
  } as RequestInit);
}

function sent(i = 0) {
  const [url, init] = fetchMock.mock.calls[i]!;
  return { url, init, headers: new Headers(init.headers) };
}

describe("forwarding", () => {
  it("sends a same-origin GET to the fixed API origin with the session attached here", async () => {
    const res = await proxyToApi(browser("/api/wheel/v1/projects?limit=5"));
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual([{ id: "p1" }]);
    expect(sent().url).toBe(`${API}/v1/projects?limit=5`);
    expect(sent().headers.get("x-auth-token")).toBe(TOKEN);
    expect(sent().init.method).toBe("GET");
  });

  it("never forwards the browser's cookies or a credential the browser supplied", async () => {
    await proxyToApi(
      browser("/api/wheel/v1/projects/p1/engine/v1/board", {
        headers: { "x-project-id": "p1", "x-auth-token": "attacker", authorization: "Bearer attacker", "x-forwarded-for": "6.6.6.6" },
      }),
    );
    const headers = sent().headers;
    expect([...headers.keys()].sort()).toEqual(["x-auth-token", "x-project-id"]);
    expect(headers.get("x-auth-token")).toBe(TOKEN);
  });

  it("is not redirected by the Host header, a forwarded host, or the URL the request arrived on", async () => {
    await proxyToApi(browser("/api/wheel/v1/projects", { headers: { host: "evil.example", "x-forwarded-host": "evil.example" } }));
    await proxyToApi(browser("/api/wheel/v1/projects", { origin: "http://evil.example" }));
    expect(new URL(sent(0).url).origin).toBe(API);
    expect(new URL(sent(1).url).origin).toBe(API);
    expect(sent(0).init.redirect).toBe("manual");
  });

  it.each(["POST", "PUT", "PATCH", "DELETE"])("streams %s with its JSON body and content-type", async (method) => {
    await proxyToApi(
      browser("/api/wheel/v1/projects/p1/engine/v1/wires", {
        method,
        body: '{"from":"a","to":"b","type":"send"}',
        headers: { "content-type": "application/json", "x-project-id": "p1" },
      }),
    );
    expect(sent().init.method).toBe(method);
    expect(sent().init.body).toBeInstanceOf(ReadableStream);
    expect(sent().init.duplex).toBe("half");
    expect(bodies[0]?.toString()).toBe('{"from":"a","to":"b","type":"send"}');
    expect(sent().headers.get("content-type")).toBe("application/json");
  });

  it("carries a raw chest blob byte for byte, streamed rather than buffered", async () => {
    const blob = new Uint8Array(1024 * 1024).map((_, i) => (i * 31) % 256);
    await proxyToApi(
      browser("/api/wheel/v1/projects/p1/engine/v1/chests/c1/blob?key=a%2Fb.bin", {
        method: "PUT",
        body: blob,
        headers: { "content-type": "application/octet-stream", "x-project-id": "p1" },
      }),
    );
    expect(sent().url).toBe(`${API}/v1/projects/p1/engine/v1/chests/c1/blob?key=a%2Fb.bin`);
    expect(sent().init.body).toBeInstanceOf(ReadableStream);
    expect(Buffer.compare(bodies[0]!, Buffer.from(blob))).toBe(0);
  });

  it("sends no body at all for a POST that has none", async () => {
    await proxyToApi(browser("/api/wheel/v1/projects/p1/start", { method: "POST" }));
    expect(sent().init.body).toBeUndefined();
  });
});

describe("answers", () => {
  it("passes the API's error envelope through unchanged, with retry-after", async () => {
    const envelope = '{"error":{"code":"ambiguous_credential","message":"two vaults define ANTHROPIC_API_KEY"}}';
    fetchMock.mockResolvedValue(
      new Response(envelope, { status: 409, headers: { "content-type": "application/json", "retry-after": "7" } }),
    );
    const res = await proxyToApi(browser("/api/wheel/v1/projects/p1/engine/v1/wires", { method: "POST", body: "{}" }));
    expect(res.status).toBe(409);
    expect(await res.text()).toBe(envelope);
    expect(res.headers.get("retry-after")).toBe("7");
    expect(res.headers.get("x-content-type-options")).toBe("nosniff");
  });

  it("streams the response instead of buffering it", async () => {
    const stream = new ReadableStream<Uint8Array>({ start: (c) => c.enqueue(new TextEncoder().encode("first chunk")) });
    fetchMock.mockResolvedValue(new Response(stream, { headers: { "content-type": "application/octet-stream" } }));
    const res = await proxyToApi(browser("/api/wheel/v1/projects/p1/engine/v1/chests/c1/blob?key=big"));
    const { value } = await res.body!.getReader().read();
    expect(new TextDecoder().decode(value)).toBe("first chunk");
  });

  it("never lets a chest's HTML render on this origin", async () => {
    fetchMock.mockResolvedValue(new Response("<script>steal()</script>", { headers: { "content-type": "text/html" } }));
    const res = await proxyToApi(browser("/api/wheel/v1/projects/p1/engine/v1/chests/c1/blob?key=x.html"));
    expect(res.headers.get("content-security-policy")).toBe("sandbox");
    expect(res.headers.get("content-disposition")).toBe("attachment");
    expect(res.headers.get("x-content-type-options")).toBe("nosniff");
  });

  it("does not hand the browser a redirect the API issued", async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 302, headers: { location: "http://evil.example/" } }));
    const res = await proxyToApi(browser("/api/wheel/v1/projects"));
    expect(res.status).toBe(302);
    expect(res.headers.get("location")).toBeNull();
  });

  it("clears the cookie when the API says the session is dead", async () => {
    fetchMock.mockResolvedValue(Response.json({ error: { code: "unauthenticated", message: "expired" } }, { status: 401 }));
    const res = await proxyToApi(browser("/api/wheel/v1/projects"));
    expect(res.status).toBe(401);
    expect(res.headers.get("set-cookie")).toMatch(/^wheel_session=; .*Max-Age=0/);
  });

  it("presents only the mock's constant in mock mode, and leaves cookies alone on a 401", async () => {
    vi.stubEnv("WHEEL_AUTH_MODE", "mock");
    vi.stubEnv("WHEEL_DEV_TOKEN", "secret-dev-token");
    fetchMock.mockResolvedValue(new Response(null, { status: 401 }));
    const res = await proxyToApi(browser("/api/wheel/v1/projects"));
    expect(res.headers.get("set-cookie")).toBeNull();
    expect(sent().headers.get("x-auth-token")).toBe("mock-session-token");
  });

  it("sends no body with a 204", async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 204 }));
    const res = await proxyToApi(browser("/api/wheel/v1/projects/p1", { method: "DELETE" }));
    expect(res.status).toBe(204);
    expect(res.body).toBeNull();
  });

  it("reports an API it cannot reach as a 502, not as the user's fault", async () => {
    fetchMock.mockRejectedValue(new TypeError("fetch failed"));
    const res = await proxyToApi(browser("/api/wheel/v1/projects"));
    expect(res.status).toBe(502);
    expect((await res.json()).error.code).toBe("api_unreachable");
  });
});

describe("refusals, none of which reach the API", () => {
  it("refuses a cross-origin POST", async () => {
    const res = await proxyToApi(
      browser("/api/wheel/v1/projects", { method: "POST", body: "{}", headers: { origin: "https://evil.example", "sec-fetch-site": "cross-site" } }),
    );
    expect(res.status).toBe(403);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("refuses a Host that may be DNS-rebound when no public origin is configured", async () => {
    vi.stubEnv("WHEEL_PUBLIC_ORIGIN", "");
    const rebound = await proxyToApi(browser("/api/wheel/v1/projects", { headers: { host: "rebound.example:3000" } }));
    expect(rebound.status).toBe(403);
    const local = await proxyToApi(
      browser("/api/wheel/v1/projects", { origin: "http://localhost:3000", headers: { host: "localhost:3000", origin: "http://localhost:3000" } }),
    );
    expect(local.status).toBe(200);
    expect(fetchMock).toHaveBeenCalledOnce();
  });

  it.each([
    ["the token routes, so a login can never be answered into page script", "/api/wheel/v1/auth/login"],
    ["a traversal out of /v1/projects", "/api/wheel/v1/projects/%2e%2e/auth/me"],
    ["a double-encoded traversal meant for a hop that decodes twice", "/api/wheel/v1/projects/p1/engine/v1/%252e%252e/%252e%252e/%252e%252e/host/v1"],
    ["the ws-ticket route, however it is spelled", "/api/wheel/v1/projects/p1/ws%2Dticket"],
  ])("refuses %s", async (_label, path) => {
    const res = await proxyToApi(browser(path, { method: "POST", body: "{}" }));
    expect(res.status).toBe(404);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("refuses a smuggled project id", async () => {
    const res = await proxyToApi(browser("/api/wheel/v1/projects", { headers: { "x-project-id": "p1, p2" } }));
    expect(res.status).toBe(400);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("refuses without a session, and clears whatever cookie was there", async () => {
    const res = await proxyToApi(browser("/api/wheel/v1/projects", { headers: { cookie: "theme=dark" } }));
    expect(res.status).toBe(401);
    expect(res.headers.get("set-cookie")).toMatch(/Max-Age=0/);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it.each([
    ["any string at all", "garbage"],
    ["an expired JWT", expiredJwt()],
  ])("treats %s as no session, without reading a byte of the body", async (_label, cookie) => {
    // highWaterMark 0, or the stream pulls once on construction and the test measures nothing.
    let pulled = false;
    const body = new ReadableStream<Uint8Array>(
      {
        pull(controller) {
          pulled = true;
          controller.enqueue(new Uint8Array(1024));
        },
      },
      { highWaterMark: 0 },
    );
    const res = await proxyToApi(
      browser("/api/wheel/v1/projects/p1/engine/v1/chests/c1/blob?key=k", { method: "PUT", body, headers: { cookie: `wheel_session=${cookie}` } }),
    );
    expect(res.status).toBe(401);
    expect(pulled).toBe(false);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("refuses a body declared over the cap without reading it", async () => {
    vi.stubEnv("WHEEL_PROXY_BODY_LIMIT_BYTES", "10");
    const res = await proxyToApi(
      browser("/api/wheel/v1/projects", { method: "POST", body: "{}", headers: { "content-length": "999999999" } }),
    );
    expect(res.status).toBe(413);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("stops a streamed body the moment it passes the cap, and says 413", async () => {
    vi.stubEnv("WHEEL_PROXY_BODY_LIMIT_BYTES", "10");
    const res = await proxyToApi(browser("/api/wheel/v1/projects", { method: "POST", body: "12345678901" }));
    expect(res.status).toBe(413);
    expect((await res.json()).error.code).toBe("payload_too_large");
  });

  it("accepts a body at exactly the cap", async () => {
    vi.stubEnv("WHEEL_PROXY_BODY_LIMIT_BYTES", "10");
    const res = await proxyToApi(browser("/api/wheel/v1/projects", { method: "POST", body: "1234567890" }));
    expect(res.status).toBe(200);
    expect(bodies[0]?.toString()).toBe("1234567890");
  });
});
