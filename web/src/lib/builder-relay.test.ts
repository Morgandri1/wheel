// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The relay is the trust boundary for a builder turn: the session is attached HERE, the browser
 * never sees a token or an address, and a stream held open is a resource this server has to bound.
 */
const upstream = vi.fn();
vi.mock("@/lib/upstream", async () => {
  const actual = await vi.importActual<typeof import("@/lib/upstream")>("@/lib/upstream");
  return {
    ...actual,
    upstreamToken: vi.fn(async () => "session-token"),
    callApi: (...args: unknown[]) => upstream(...args),
  };
});
vi.mock("@/lib/runtime-config", async () => {
  const actual = await vi.importActual<typeof import("@/lib/runtime-config")>("@/lib/runtime-config");
  return {
    ...actual,
    serverApiBaseUrl: () => "https://api.wheel.test",
    serverAuthMode: () => "local" as const,
    proxyBodyLimit: () => 1024,
    // Named, so the same-origin check has an origin to compare against rather than refusing
    // every non-loopback host (`same-origin.ts`).
    publicOriginSetting: () => "https://app.wheel.test",
    trustProxy: () => false,
  };
});

import { StreamSlots } from "@/lib/event-relay";
import { relayBuilderTurn } from "@/lib/builder-relay";
import { upstreamToken } from "@/lib/upstream";

const PROJECT = "11111111-1111-4111-8111-111111111111";

function request(body: unknown, origin = "https://app.wheel.test"): Request {
  return new Request(`https://app.wheel.test/api/wheel/projects/${PROJECT}/builder`, {
    method: "POST",
    headers: { origin, host: "app.wheel.test", "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

function sseResponse(text: string): Response {
  return new Response(text, { status: 200, headers: { "content-type": "text/event-stream" } });
}

beforeEach(() => {
  upstream.mockReset();
  vi.mocked(upstreamToken).mockResolvedValue("session-token");
});
afterEach(() => vi.restoreAllMocks());

describe("relaying a builder turn", () => {
  it("calls the API with the session and streams the frames back", async () => {
    upstream.mockImplementation(async () => sseResponse("event: delta\ndata: {\"text\":\"hi\"}\n\n"));
    const res = await relayBuilderTurn(request({ mode: "new", turns: [] }), PROJECT);

    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toMatch(/text\/event-stream/);
    // A hop that buffers turns a stream into one late response, which is the thing this avoids.
    expect(res.headers.get("cache-control")).toMatch(/no-transform/);
    expect(res.headers.get("x-accel-buffering")).toBe("no");
    expect(await res.text()).toContain('data: {"text":"hi"}');

    const [target, call] = upstream.mock.calls[0] as [string, { token: string; method: string }];
    expect(target).toBe(`https://api.wheel.test/v1/projects/${PROJECT}/builder/turns`);
    expect(call.token).toBe("session-token");
    expect(call.method).toBe("POST");
  });

  it("hands back a refusal that arrived instead of a stream, status and all", async () => {
    upstream.mockResolvedValue(
      new Response(JSON.stringify({ error: { code: "needs_auth", message: "no credential" } }), {
        status: 409,
        headers: { "content-type": "application/json" },
      }),
    );
    const res = await relayBuilderTurn(request({ mode: "new", turns: [] }), PROJECT);
    expect(res.status).toBe(409);
    expect(await res.json()).toMatchObject({ error: { code: "needs_auth" } });
  });

  it("refuses a signed-out caller before it reaches the API", async () => {
    vi.mocked(upstreamToken).mockResolvedValue(null);
    const res = await relayBuilderTurn(request({ mode: "new", turns: [] }), PROJECT);
    expect(res.status).toBe(401);
    expect(upstream).not.toHaveBeenCalled();
  });

  it("refuses a cross-origin post before it reaches the API", async () => {
    const res = await relayBuilderTurn(request({ mode: "new", turns: [] }, "https://evil.test"), PROJECT);
    expect(res.status).toBeGreaterThanOrEqual(400);
    expect(upstream).not.toHaveBeenCalled();
  });

  it("refuses a project id that is not one", async () => {
    const res = await relayBuilderTurn(request({ mode: "new", turns: [] }), "../../auth/me");
    expect(res.status).toBe(404);
    expect(upstream).not.toHaveBeenCalled();
  });

  it("refuses a body past the cap rather than forwarding it", async () => {
    const res = await relayBuilderTurn(
      request({ mode: "new", turns: [{ role: "user", text: "x".repeat(4096) }] }),
      PROJECT,
    );
    expect(res.status).toBe(413);
    expect(upstream).not.toHaveBeenCalled();
  });

  it("refuses once this session is already holding its share of streams", async () => {
    upstream.mockImplementation(async () => sseResponse("event: done\ndata: {}\n\n"));
    const slots = new StreamSlots({ perSession: 1, perAddress: 8, total: 8 });
    // Take the session's only slot and keep it: an unread stream is still an open one.
    const held = await relayBuilderTurn(request({ mode: "new", turns: [] }), PROJECT, slots);
    expect(held.status).toBe(200);

    const refused = await relayBuilderTurn(request({ mode: "new", turns: [] }), PROJECT, slots);
    expect(refused.status).toBe(429);
    expect(await refused.json()).toMatchObject({ error: { code: "too_many_streams" } });
  });

  it("gives the slot back when the stream is read to the end", async () => {
    upstream.mockImplementation(async () => sseResponse("event: done\ndata: {}\n\n"));
    const slots = new StreamSlots({ perSession: 1, perAddress: 8, total: 8 });
    const first = await relayBuilderTurn(request({ mode: "new", turns: [] }), PROJECT, slots);
    await first.text();

    const second = await relayBuilderTurn(request({ mode: "new", turns: [] }), PROJECT, slots);
    expect(second.status).toBe(200);
  });

  it("says the API is unreachable rather than pretending the builder answered", async () => {
    upstream.mockResolvedValue({ failure: "unreachable" });
    const res = await relayBuilderTurn(request({ mode: "new", turns: [] }), PROJECT);
    expect(res.status).toBe(502);
  });
});
