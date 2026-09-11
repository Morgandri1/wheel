// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it } from "vitest";
import {
  apiTarget,
  cappedStream,
  declaredLength,
  errorEnvelope,
  forwardedRequestHeaders,
  isJsonMediaType,
  readCapped,
  readUpTo,
  returnedResponseHeaders,
  safeSegment,
  upstreamPath,
} from "./proxy-rules";

function streamOf(...chunks: number[]): { stream: ReadableStream<Uint8Array>; pulls: () => number; cancelled: () => boolean } {
  let i = 0;
  let cancelled = false;
  const stream = new ReadableStream<Uint8Array>(
    {
      pull(controller) {
        const size = chunks[i++];
        if (size === undefined) controller.close();
        else controller.enqueue(new Uint8Array(size).fill(i));
      },
      cancel() {
        cancelled = true;
      },
    },
    { highWaterMark: 0 },
  );
  return { stream, pulls: () => i, cancelled: () => cancelled };
}

describe("which paths reach the API", () => {
  it.each([
    ["/api/wheel/v1/projects", "/v1/projects"],
    ["/api/wheel/v1/projects/instantiate", "/v1/projects/instantiate"],
    ["/api/wheel/v1/projects/9b1d-44", "/v1/projects/9b1d-44"],
    ["/api/wheel/v1/projects/p1/start", "/v1/projects/p1/start"],
    ["/api/wheel/v1/projects/p1/stop", "/v1/projects/p1/stop"],
    ["/api/wheel/v1/projects/p1/restart", "/v1/projects/p1/restart"],
    ["/api/wheel/v1/projects/p1/board/apply", "/v1/projects/p1/board/apply"],
    ["/api/wheel/v1/projects/9b1d/engine/v1/board", "/v1/projects/9b1d/engine/v1/board"],
    ["/api/wheel/v1/projects/p1/engine/v1/vault/n1/ANTHROPIC_API_KEY", "/v1/projects/p1/engine/v1/vault/n1/ANTHROPIC_API_KEY"],
    ["/api/wheel/v1/projects/p1/engine/v1/chests/c1/blob", "/v1/projects/p1/engine/v1/chests/c1/blob"],
  ])("forwards %s as %s", (pathname, expected) => {
    expect(upstreamPath(pathname)).toBe(expected);
  });

  it.each([
    // The token routes: answered through here, a login would put the JWT in page script's hands.
    "/api/wheel/v1/auth/login",
    "/api/wheel/v1/auth/signup",
    "/api/wheel/v1/auth/me",
    "/api/wheel/healthz",
    "/api/wheel/v1/host/healthz",
    "/api/wheel/v2/projects",
    "/api/wheel/p/p1/hook",
    "/api/wheelx/v1/projects",
    "/api/wheel",
    "/api/wheel/",
    "/v1/projects",
  ])("refuses %s", (pathname) => {
    expect(upstreamPath(pathname)).toBeNull();
  });

  // A positive list of the board's routes, so any route not on it is refused however it is spelled.
  it.each([
    "/api/wheel/v1/projects/p1/ws-ticket",
    "/api/wheel/v1/projects/p1/ws%2Dticket",
    "/api/wheel/v1/projects/p1/ws%2dticket",
    "/api/wheel/v1/projects/p1/ws-ticket;x",
    "/api/wheel/v1/projects/p1/WS-TICKET",
    "/api/wheel/v1/projects/p1/board",
    "/api/wheel/v1/projects/p1/board/apply/extra",
    "/api/wheel/v1/projects/p1/start/now",
    "/api/wheel/v1/projects/p1/engine",
    "/api/wheel/v1/projects/p1/engine/v1",
    "/api/wheel/v1/projects/p1/engine/v2/board",
    "/api/wheel/v1/projects/p1/anything",
    "/api/wheel/%76%31/projects",
    "/api/wheel/v1/%70rojects",
  ])("refuses the route %s", (pathname) => {
    expect(upstreamPath(pathname)).toBeNull();
  });

  it.each([
    ["a dot-dot segment", "/api/wheel/v1/projects/../auth/login"],
    ["a single dot", "/api/wheel/v1/projects/./x"],
    ["an encoded dot-dot", "/api/wheel/v1/projects/%2e%2e/auth/login"],
    ["an encoded dot-dot in mixed case", "/api/wheel/v1/projects/p1/engine/v1/%2E%2e/x"],
    ["half an encoded dot-dot", "/api/wheel/v1/projects/p1/engine/v1/.%2e/x"],
    ["a double-encoded dot-dot", "/api/wheel/v1/projects/p1/engine/v1/%252e%252e/%252e%252e/auth"],
    ["a double-encoded slash", "/api/wheel/v1/projects/p1/engine/v1/a%252fb"],
    ["an encoded percent", "/api/wheel/v1/projects/p1/engine/v1/100%25"],
    ["an encoded slash", "/api/wheel/v1/projects/p1/engine/v1/a%2F..%2Fb"],
    ["an encoded backslash", "/api/wheel/v1/projects/p1/engine/v1/a%5Cb"],
    ["an encoded NUL", "/api/wheel/v1/projects/p1/engine/v1/a%00"],
    ["an encoded newline", "/api/wheel/v1/projects/p1/engine/v1/a%0d%0aX-Evil:1"],
    ["a malformed escape", "/api/wheel/v1/projects/p1/engine/v1/%zz"],
    ["an empty middle segment", "/api/wheel/v1/projects/p1//engine/v1/board"],
    ["a trailing slash", "/api/wheel/v1/projects/"],
    ["a raw space", "/api/wheel/v1/projects/p1/engine/v1/a b"],
    ["a raw backslash", "/api/wheel/v1/projects/p1\\..\\auth"],
    ["a semicolon", "/api/wheel/v1/projects/p1/engine/v1/board;x"],
  ])("refuses a path with %s", (_label, pathname) => {
    expect(upstreamPath(pathname)).toBeNull();
  });

  it.each<[string, boolean]>([
    ["hello%20world", true],
    ["ANTHROPIC_API_KEY", true],
    ["a%2Fb", false],
    ["%2e", false],
    ["%2e%2e", false],
    ["%252e", false],
    ["%25", false],
    ["abc%", false],
    ["", false],
  ])("safeSegment(%j) is %s", (segment, ok) => {
    expect(safeSegment(segment)).toBe(ok);
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
      "content-type",
      "etag",
      "last-modified",
      "retry-after",
      "x-content-type-options",
      "x-wheel-mock",
    ]);
    expect(out.get("x-content-type-options")).toBe("nosniff");
  });

  it.each(["text/html; charset=utf-8", "image/svg+xml", "application/octet-stream", null])(
    "sandboxes a %s body and downloads it instead of rendering it on this origin",
    (type) => {
      const out = returnedResponseHeaders(new Headers(type ? { "content-type": type } : {}));
      expect(out.get("content-security-policy")).toBe("sandbox");
      expect(out.get("content-disposition")).toBe("attachment");
      expect(out.get("x-content-type-options")).toBe("nosniff");
    },
  );

  it("keeps an upstream attachment and its filename, and turns inline into attachment", () => {
    const named = 'attachment; filename="notes.html"';
    expect(returnedResponseHeaders(new Headers({ "content-type": "text/html", "content-disposition": named })).get("content-disposition")).toBe(named);
    expect(returnedResponseHeaders(new Headers({ "content-type": "text/html", "content-disposition": "inline" })).get("content-disposition")).toBe("attachment");
  });

  it.each(["application/json", "application/json; charset=utf-8", "application/problem+json"])("leaves JSON (%s) to be read", (type) => {
    const out = returnedResponseHeaders(new Headers({ "content-type": type }));
    expect(out.has("content-security-policy")).toBe(false);
    expect(out.has("content-disposition")).toBe(false);
  });

  it.each<[string | null, boolean]>([
    ["application/json", true],
    ["Application/JSON; charset=utf-8", true],
    ["application/vnd.api+json", true],
    ["text/plain", false],
    ["application/jsonp", false],
    ["", false],
    [null, false],
  ])("isJsonMediaType(%j) is %s", (type, ok) => {
    expect(isJsonMediaType(type)).toBe(ok);
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

  it("streams a body at exactly the limit through untouched", async () => {
    const { transform, exceeded } = cappedStream(10);
    const out = await new Response(streamOf(4, 6).stream.pipeThrough(transform)).arrayBuffer();
    expect(out.byteLength).toBe(10);
    expect(exceeded()).toBe(false);
  });

  it("errors a streamed body the moment it passes the limit, without reading the rest", async () => {
    const source = streamOf(8, 8, 8, 8, 8, 8);
    const { transform, exceeded } = cappedStream(10);
    await expect(new Response(source.stream.pipeThrough(transform)).arrayBuffer()).rejects.toThrow();
    expect(exceeded()).toBe(true);
    expect(source.pulls()).toBeLessThan(6);
  });

  it("returns a body it reads itself at exactly the limit whole", async () => {
    expect((await readCapped(streamOf(4, 6).stream, 10))?.byteLength).toBe(10);
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
