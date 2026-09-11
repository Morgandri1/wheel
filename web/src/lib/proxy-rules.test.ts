// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it, vi } from "vitest";
import {
  apiTarget,
  declaredLength,
  errorEnvelope,
  forwardedRequestHeaders,
  readCapped,
  readUpTo,
  returnedResponseHeaders,
  safeSegment,
  upstreamPath,
} from "./proxy-rules";

function streamOf(...chunks: number[]): { stream: ReadableStream<Uint8Array>; pulls: () => number; cancelled: () => boolean } {
  let i = 0;
  let cancelled = false;
  const stream = new ReadableStream<Uint8Array>({
    pull(controller) {
      const size = chunks[i++];
      if (size === undefined) controller.close();
      else controller.enqueue(new Uint8Array(size).fill(i));
    },
    cancel() {
      cancelled = true;
    },
  }, { highWaterMark: 0 });
  return { stream, pulls: () => i, cancelled: () => cancelled };
}

describe("which paths reach the API", () => {
  it.each([
    ["/api/wheel/v1/projects", "/v1/projects"],
    ["/api/wheel/v1/projects/instantiate", "/v1/projects/instantiate"],
    ["/api/wheel/v1/projects/9b1d/engine/v1/board", "/v1/projects/9b1d/engine/v1/board"],
    ["/api/wheel/v1/projects/p1/engine/v1/vault/n1/ANTHROPIC_API_KEY", "/v1/projects/p1/engine/v1/vault/n1/ANTHROPIC_API_KEY"],
    ["/api/wheel/v1/projects/p1/engine/v1/agents/a1/ws-ticket", "/v1/projects/p1/engine/v1/agents/a1/ws-ticket"],
  ])("forwards %s as %s", (pathname, expected) => {
    expect(upstreamPath(pathname)).toBe(expected);
  });

  it.each([
    // The token routes: answered through here, a login would put the JWT in page script's hands.
    "/api/wheel/v1/auth/login",
    "/api/wheel/v1/auth/signup",
    "/api/wheel/v1/auth/me",
    // Not the board's business.
    "/api/wheel/healthz",
    "/api/wheel/v1/host/healthz",
    "/api/wheel/v2/projects",
    "/api/wheel/p/p1/hook",
    // The ticket route stays on the API for other clients; the web never mints one.
    "/api/wheel/v1/projects/p1/ws-ticket",
    // Prefix confusion.
    "/api/wheelx/v1/projects",
    "/api/wheel",
    "/api/wheel/",
    "/v1/projects",
  ])("refuses %s", (pathname) => {
    expect(upstreamPath(pathname)).toBeNull();
  });

  it.each([
    ["a dot-dot segment", "/api/wheel/v1/projects/../auth/login"],
    ["a single dot", "/api/wheel/v1/projects/./x"],
    ["an encoded dot-dot", "/api/wheel/v1/projects/%2e%2e/auth/login"],
    ["an encoded dot-dot in capitals", "/api/wheel/v1/projects/%2E%2E/auth"],
    ["an encoded slash", "/api/wheel/v1/projects/p1/a%2F..%2Fb"],
    ["an encoded backslash", "/api/wheel/v1/projects/p1/a%5Cb"],
    ["an encoded NUL", "/api/wheel/v1/projects/p1/a%00"],
    ["an encoded newline", "/api/wheel/v1/projects/p1/a%0d%0aX-Evil:1"],
    ["a malformed escape", "/api/wheel/v1/projects/p1/%zz"],
    ["an empty segment", "/api/wheel/v1/projects/p1//x"],
    ["a trailing slash", "/api/wheel/v1/projects/"],
    ["a raw space", "/api/wheel/v1/projects/p1/a b"],
    ["a raw backslash", "/api/wheel/v1/projects/p1\\..\\auth"],
  ])("refuses a path with %s", (_label, pathname) => {
    expect(upstreamPath(pathname)).toBeNull();
  });

  it("accepts a segment with ordinary escapes", () => {
    expect(safeSegment("hello%20world")).toBe(true);
    expect(safeSegment("a%2Fb")).toBe(false);
  });
});

describe("where a forwarded request lands", () => {
  it("is always the configured origin", () => {
    expect(apiTarget("http://127.0.0.1:8080", "/v1/projects", "?a=1")).toBe("http://127.0.0.1:8080/v1/projects?a=1");
  });

  it("keeps a base path the API sits under", () => {
    expect(apiTarget("https://gw.example/wheel", "/v1/projects")).toBe("https://gw.example/wheel/v1/projects");
  });

  it.each([
    ["dot segments", "/v1/projects/../../admin"],
    ["encoded dot segments", "/v1/projects/%2e%2e/%2e%2e/admin"],
  ])("refuses a path that URL parsing would move (%s)", (_label, path) => {
    expect(apiTarget("http://api.internal:8080", path)).toBeNull();
  });

  it.each([
    ["an authority-looking path", "//evil.example/x", ""],
    ["an @ in the query", "/v1/projects", "?@evil.example"],
    ["a full URL in the query", "/v1/projects", "?next=http://evil.example/"],
  ])("cannot be pointed at another host by %s", (_label, path, search) => {
    const target = apiTarget("http://api.internal:8080", path, search);
    expect(target === null || new URL(target).origin === "http://api.internal:8080").toBe(true);
  });
});

describe("which request headers go upstream", () => {
  it("only x-project-id and content-type — never cookies or a client-supplied credential", () => {
    const out = forwardedRequestHeaders(
      new Headers({
        "x-project-id": "9b1d-44",
        "content-type": "application/json",
        cookie: "wheel_session=secret",
        "x-auth-token": "attacker-token",
        authorization: "Bearer attacker",
        "x-forwarded-for": "10.0.0.1",
        "x-forwarded-host": "evil.example",
        origin: "http://wheel.test",
        accept: "*/*",
      }),
    );
    expect(out && [...out.keys()].sort()).toEqual(["content-type", "x-project-id"]);
  });

  it("forwards nothing when there is nothing to forward", () => {
    expect([...forwardedRequestHeaders(new Headers())!.keys()]).toEqual([]);
  });

  it.each([
    ["two values folded into one", { "x-project-id": "p1, p2" }],
    ["a path in the project id", { "x-project-id": "../x" }],
    ["a space in the project id", { "x-project-id": "a b" }],
    ["an overlong project id", { "x-project-id": "a".repeat(65) }],
    ["an overlong content-type", { "content-type": `text/plain; ${"x".repeat(300)}` }],
    ["a non-ASCII content-type", { "content-type": "text/plain; charset=ü" }],
  ])("refuses %s", (_label, headers) => {
    expect(forwardedRequestHeaders(new Headers(headers))).toBeNull();
  });
});

describe("which response headers come back", () => {
  it("passes what the browser uses and drops what could lie or set state", () => {
    const out = returnedResponseHeaders(
      new Headers({
        "content-type": "application/json",
        "content-disposition": "attachment; filename=a.txt",
        "retry-after": "30",
        "cache-control": "no-store",
        etag: '"x"',
        "last-modified": "Wed, 01 Jan 2026 00:00:00 GMT",
        "x-wheel-mock": "no-tenancy",
        "set-cookie": "planted=1",
        "content-length": "999",
        "content-encoding": "gzip",
        location: "http://evil.example/",
        "access-control-allow-origin": "*",
        "x-powered-by": "axum",
      }),
    );
    expect([...out.keys()].sort()).toEqual([
      "cache-control",
      "content-disposition",
      "content-type",
      "etag",
      "last-modified",
      "retry-after",
      "x-wheel-mock",
    ]);
  });
});

describe("body size", () => {
  it.each<[string | null, number]>([
    ["123", 123],
    [null, 0],
    ["lots", 0],
    ["-1", 0],
  ])("reads a declared length of %j as %i", (value, expected) => {
    expect(declaredLength(new Headers(value === null ? {} : { "content-length": value }))).toBe(expected);
  });

  it("returns a body at exactly the limit whole", async () => {
    const { stream } = streamOf(4, 6);
    const bytes = await readCapped(stream, 10);
    expect(bytes?.byteLength).toBe(10);
  });

  it("refuses one byte more, counted across chunks", async () => {
    expect(await readCapped(streamOf(4, 4, 3).stream, 10)).toBeNull();
  });

  it("stops reading and cancels the source the moment the cap is passed", async () => {
    const source = streamOf(8, 8, 8, 8, 8);
    const { bytes, truncated } = await readUpTo(source.stream, 10);
    expect(truncated).toBe(true);
    expect(bytes.byteLength).toBe(10);
    expect(source.cancelled()).toBe(true);
    expect(source.pulls()).toBe(2);
  });

  it("keeps the bytes in order", async () => {
    const { bytes } = await readUpTo(streamOf(2, 3).stream, 100);
    expect([...bytes]).toEqual([1, 1, 2, 2, 2]);
  });

  it("reads no body as empty", async () => {
    expect((await readCapped(null, 10))?.byteLength).toBe(0);
  });
});

describe("errorEnvelope", () => {
  it("speaks the API's error shape", async () => {
    const res = errorEnvelope(413, "payload_too_large", "too big", { "x-test": "1" });
    expect(res.status).toBe(413);
    expect(res.headers.get("x-test")).toBe("1");
    expect(await res.json()).toEqual({ error: { code: "payload_too_large", message: "too big" } });
  });
});

describe("the instrument", () => {
  it("really exercises a stream that would keep going", async () => {
    const endless = new ReadableStream<Uint8Array>({ pull: (c) => c.enqueue(new Uint8Array(1024)) });
    const spy = vi.fn();
    const result = await readCapped(endless, 4096).then((r) => (spy(), r));
    expect(result).toBeNull();
    expect(spy).toHaveBeenCalledOnce();
  });
});
