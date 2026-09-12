// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, describe, expect, it, vi } from "vitest";
import { BuilderStopped, builderCredential, builderTurns } from "./builder-client";
import type { BuilderFrame } from "./builder-stream";

const PROJECT = "11111111-1111-4111-8111-111111111111";

function sse(body: string): Response {
  return new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } });
}

function mockFetch(impl: (url: string, init: RequestInit) => Response | Promise<Response>) {
  const fetchMock = vi.fn(async (url: unknown, init: unknown) =>
    impl(String(url), (init ?? {}) as RequestInit),
  );
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

async function collect(projectId = PROJECT): Promise<BuilderFrame[]> {
  const frames: BuilderFrame[] = [];
  for await (const frame of builderTurns(projectId, { mode: "new", turns: [] })) frames.push(frame);
  return frames;
}

afterEach(() => vi.unstubAllGlobals());

describe("a builder turn, over this app's own server", () => {
  it("posts to this origin and yields the frames it streams back", async () => {
    const fetchMock = mockFetch(() =>
      sse('event: delta\ndata: {"text":"hi"}\n\nevent: done\ndata: {"text":"hi","boards":1}\n\n'),
    );
    expect(await collect()).toEqual([
      { kind: "delta", text: "hi" },
      { kind: "done", text: "hi", boards: 1 },
    ]);

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe(`/api/wheel/projects/${PROJECT}/builder`);
    expect(init.method).toBe("POST");
    expect(init.credentials).toBe("same-origin");
    // The browser holds no credential of its own: the session is a cookie this app set.
    expect(JSON.stringify(init.headers)).not.toMatch(/auth-token/i);
  });

  /**
   * A stream that stops without a terminal frame is not a finished answer. Reporting it as one
   * would leave half a proposal on screen looking complete.
   */
  it("says so when the stream stops partway", async () => {
    mockFetch(() => sse('event: delta\ndata: {"text":"half an ans"}\n\n'));
    const frames = await collect();
    expect(frames.at(-1)).toEqual({
      kind: "error",
      code: "stream_broken",
      message: expect.stringContaining("stopped partway"),
    });
  });

  it("throws the refusal that arrived instead of a stream, so the caller can act on it", async () => {
    mockFetch(
      () =>
        new Response(
          JSON.stringify({
            error: { code: "needs_auth", message: "no credential" },
            sources: { agents: [{ id: "a1", name: "worker" }], vaults: [] },
          }),
          { status: 409, headers: { "content-type": "application/json" } },
        ),
    );
    const error = await collect().catch((e: unknown) => e);
    expect(error).toBeInstanceOf(BuilderStopped);
    expect((error as BuilderStopped).refusal.code).toBe("needs_auth");
    expect((error as BuilderStopped).refusal.sources?.agents[0]?.name).toBe("worker");
  });

  it("refuses a 200 that is not a stream rather than hanging on it", async () => {
    mockFetch(() => new Response("{}", { status: 200, headers: { "content-type": "application/json" } }));
    await expect(collect()).rejects.toBeInstanceOf(BuilderStopped);
  });

  it("says the app's own server is unreachable when the fetch itself fails", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => { throw new TypeError("Failed to fetch"); }));
    const error = (await collect().catch((e: unknown) => e)) as BuilderStopped;
    expect(error.refusal.code).toBe("offline");
  });
});

describe("the builder's own credential", () => {
  /**
   * The proxy refuses a `%` that survives one decode, so a path built from ids must not arrive
   * pre-encoded. A UUID encodes to itself; this pins that nothing here adds an escape.
   */
  it("addresses the engine through the proxy with no percent-encoding in the path", async () => {
    const fetchMock = mockFetch(() => Response.json({ configured: true, kind: "api_key" }));
    await builderCredential.get(PROJECT);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe(`/api/wheel/v1/projects/${PROJECT}/engine/v1/builder/credential`);
    expect(url).not.toContain("%");
    expect((init.headers as Record<string, string>)["x-project-id"]).toBe(PROJECT);
  });

  it("sends what was typed under the field it belongs in", async () => {
    const fetchMock = mockFetch(() => Response.json({ configured: true, kind: "oauth_token" }));
    await builderCredential.put(PROJECT, { setup_token: "sk-ant-oat01-x" });
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(String(init.body))).toEqual({ setup_token: "sk-ant-oat01-x" });
    expect(init.method).toBe("PUT");
  });

  it("surfaces the engine's own refusal rather than a generic failure", async () => {
    mockFetch(
      () =>
        new Response(JSON.stringify({ error: { code: "policy", message: "this project is api-key-only" } }), {
          status: 403,
          headers: { "content-type": "application/json" },
        }),
    );
    await expect(builderCredential.put(PROJECT, { api_key: "sk-ant-oat01-x" })).rejects.toMatchObject({
      status: 403,
      code: "policy",
      message: expect.stringContaining("api-key-only"),
    });
  });

  it("refuses a project id that is not one, before any request", async () => {
    const fetchMock = mockFetch(() => Response.json({}));
    await expect(builderCredential.get("../../auth/me")).rejects.toMatchObject({ status: 404 });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("takes a 204 as done rather than trying to read a body", async () => {
    mockFetch(() => new Response(null, { status: 204 }));
    await expect(builderCredential.remove(PROJECT)).resolves.toBeUndefined();
  });
});
