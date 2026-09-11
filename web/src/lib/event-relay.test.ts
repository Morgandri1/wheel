// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { EventEmitter, once } from "node:events";
import type { AddressInfo } from "node:net";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WebSocketServer } from "ws";
import {
  HEARTBEAT_MS,
  SSE_HEADERS,
  SSE_HEARTBEAT,
  eventsSocketUrl,
  relayEvents,
  sseData,
  sseEvent,
  type Connect,
} from "./event-relay";

class FakeSocket extends EventEmitter {
  terminate = vi.fn();
}

function harness() {
  const opened: { url: string; headers: Record<string, string>; socket: FakeSocket }[] = [];
  const connect: Connect = (url, headers) => {
    const socket = new FakeSocket();
    opened.push({ url, headers, socket });
    return socket as unknown as ReturnType<Connect>;
  };
  return { opened, connect };
}

function browser(init: { signal?: AbortSignal; headers?: Record<string, string> } = {}) {
  return new Request("http://wheel.test/api/wheel/projects/p1/events", {
    signal: init.signal,
    headers: { host: "wheel.test", "sec-fetch-site": "same-origin", cookie: "wheel_session=tok", ...init.headers },
  });
}

async function readAll(res: Response): Promise<string> {
  return new TextDecoder().decode(await new Response(res.body).arrayBuffer());
}

beforeEach(() => {
  vi.stubEnv("WHEEL_API_URL", "http://api.test:8080");
  vi.stubEnv("WHEEL_AUTH_MODE", "local");
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllEnvs();
});

describe("SSE framing", () => {
  it("puts one frame on one data line", () => {
    expect(sseData('{"a":1}')).toBe('data: {"a":1}\n\n');
  });

  it("splits a frame with newlines into data lines the browser joins back together", () => {
    expect(sseData("one\ntwo\r\nthree\rfour")).toBe("data: one\ndata: two\ndata: three\ndata: four\n\n");
  });

  it("names the relay's own events", () => {
    expect(sseEvent("wheel-error", { status: 401 })).toBe('event: wheel-error\ndata: {"status":401}\n\n');
  });

  it("is a comment for the heartbeat, which EventSource ignores", () => {
    expect(SSE_HEARTBEAT.startsWith(":")).toBe(true);
  });

  it("asks proxies and compression not to buffer it", () => {
    expect(SSE_HEADERS["content-type"]).toMatch(/^text\/event-stream/);
    expect(SSE_HEADERS["cache-control"]).toContain("no-transform");
  });

  it("aims the socket at the same API, over ws or wss", () => {
    expect(eventsSocketUrl("http://api:8080", "p1")).toBe("ws://api:8080/v1/projects/p1/engine/v1/events");
    expect(eventsSocketUrl("https://api.example", "p1")).toBe("wss://api.example/v1/projects/p1/engine/v1/events");
  });
});

describe("the relay", () => {
  it("opens the upstream with the session in a header and no credential in the URL", async () => {
    const { opened, connect } = harness();
    const res = await relayEvents(browser(), "p1", connect);
    expect(res.headers.get("content-type")).toMatch(/^text\/event-stream/);
    expect(opened).toHaveLength(1);
    expect(opened[0]!.url).toBe("ws://api.test:8080/v1/projects/p1/engine/v1/events");
    expect(opened[0]!.headers).toEqual({ "x-auth-token": "tok", "x-project-id": "p1" });
    opened[0]!.socket.emit("close");
  });

  it("says open only once upstream is connected, then relays each frame verbatim", async () => {
    const { opened, connect } = harness();
    const res = await relayEvents(browser(), "p1", connect);
    const socket = opened[0]!.socket;
    const frame = '{"type":"message","z":1,"a":{"body":"line one\\nline two"}}';
    socket.emit("open");
    socket.emit("message", Buffer.from(frame));
    socket.emit("message", [Buffer.from('{"type":'), Buffer.from('"lagged"}')]);
    socket.emit("message", new TextEncoder().encode('{"type":"x"}').buffer);
    socket.emit("close");
    expect(await readAll(res)).toBe(
      `${SSE_HEARTBEAT}event: wheel-open\ndata: {}\n\ndata: ${frame}\n\ndata: {"type":"lagged"}\n\ndata: {"type":"x"}\n\n`,
    );
  });

  it("sends a heartbeat every 15 seconds and stops when the stream ends", async () => {
    vi.useFakeTimers();
    const { opened, connect } = harness();
    const res = await relayEvents(browser(), "p1", connect);
    vi.advanceTimersByTime(HEARTBEAT_MS * 2);
    opened[0]!.socket.emit("close");
    vi.advanceTimersByTime(HEARTBEAT_MS * 4);
    expect(await readAll(res)).toBe(SSE_HEARTBEAT.repeat(3));
  });

  it("turns a refused handshake into wheel-error with the upstream status, and hangs up", async () => {
    const { opened, connect } = harness();
    const res = await relayEvents(browser(), "p1", connect);
    opened[0]!.socket.emit("unexpected-response", {}, { statusCode: 401 });
    expect(await readAll(res)).toBe(`${SSE_HEARTBEAT}event: wheel-error\ndata: {"status":401}\n\n`);
    expect(opened[0]!.socket.terminate).toHaveBeenCalled();
  });

  it("reports a failure before open as 502, and a failure after open as a plain end", async () => {
    const before = harness();
    const early = await relayEvents(browser(), "p1", before.connect);
    before.opened[0]!.socket.emit("error", new Error("ECONNREFUSED"));
    expect(await readAll(early)).toContain('event: wheel-error\ndata: {"status":502}');

    const after = harness();
    const late = await relayEvents(browser(), "p1", after.connect);
    after.opened[0]!.socket.emit("open");
    after.opened[0]!.socket.emit("error", new Error("reset"));
    expect(await readAll(late)).not.toContain("wheel-error");
  });

  it("closes upstream when the browser goes away", async () => {
    const aborter = new AbortController();
    const { opened, connect } = harness();
    await relayEvents(browser({ signal: aborter.signal }), "p1", connect);
    aborter.abort();
    expect(opened[0]!.socket.terminate).toHaveBeenCalled();
  });

  it("closes upstream when the reader cancels", async () => {
    const { opened, connect } = harness();
    const res = await relayEvents(browser(), "p1", connect);
    await res.body!.cancel();
    expect(opened[0]!.socket.terminate).toHaveBeenCalled();
  });

  it("hangs up on a reader that has fallen a megabyte behind, so it reconnects and backfills", async () => {
    const { opened, connect } = harness();
    await relayEvents(browser(), "p1", connect);
    const socket = opened[0]!.socket;
    socket.emit("open");
    socket.emit("message", Buffer.alloc(600 * 1024, 97));
    expect(socket.terminate).not.toHaveBeenCalled();
    socket.emit("message", Buffer.alloc(600 * 1024, 97));
    expect(socket.terminate).toHaveBeenCalled();
  });

  it("says 401 and never connects without a session", async () => {
    const { opened, connect } = harness();
    const res = await relayEvents(browser({ headers: { cookie: "" } }), "p1", connect);
    expect(await readAll(res)).toBe(`event: wheel-error\ndata: {"status":401}\n\n`);
    expect(opened).toHaveLength(0);
  });

  it("says 502 when the socket cannot even be constructed", async () => {
    const res = await relayEvents(browser(), "p1", () => {
      throw new Error("bad url");
    });
    expect(await readAll(res)).toContain('data: {"status":502}');
  });

  it("does nothing for a request that was already abandoned", async () => {
    const aborter = new AbortController();
    aborter.abort();
    const { opened, connect } = harness();
    const res = await relayEvents(browser({ signal: aborter.signal }), "p1", connect);
    expect(opened).toHaveLength(0);
    expect(await readAll(res)).toBe("");
  });

  it("refuses a cross-site request and an id that is not an id, without connecting", async () => {
    const { opened, connect } = harness();
    const cross = await relayEvents(browser({ headers: { "sec-fetch-site": "cross-site" } }), "p1", connect);
    const bad = await relayEvents(browser(), "p1/../../auth", connect);
    expect(cross.status).toBe(403);
    expect(bad.status).toBe(404);
    expect(opened).toHaveLength(0);
  });
});

describe("against a real WebSocket server", () => {
  let wss: WebSocketServer;
  const seen: { url?: string; token?: string }[] = [];

  beforeEach(async () => {
    seen.length = 0;
    wss = new WebSocketServer({
      port: 0,
      host: "127.0.0.1",
      verifyClient: (info, done) => {
        const token = info.req.headers["x-auth-token"] as string | undefined;
        seen.push({ url: info.req.url, token });
        done(token === "good", 401);
      },
    });
    await once(wss, "listening");
    vi.stubEnv("WHEEL_API_URL", `http://127.0.0.1:${(wss.address() as AddressInfo).port}`);
  });

  afterEach(async () => {
    for (const client of wss.clients) client.terminate();
    await new Promise((resolve) => wss.close(resolve));
  });

  it("authenticates by header, relays what the server sends, and ends when it closes", async () => {
    wss.on("connection", (ws) => {
      ws.send('{"type":"board.changed"}');
      ws.close();
    });
    const res = await relayEvents(browser({ headers: { cookie: "wheel_session=good" } }), "p1");
    const text = await readAll(res);
    expect(seen).toEqual([{ url: "/v1/projects/p1/engine/v1/events", token: "good" }]);
    expect(text).toContain("event: wheel-open");
    expect(text).toContain('data: {"type":"board.changed"}');
  });

  it("turns a refused handshake into wheel-error with the server's status", async () => {
    const res = await relayEvents(browser({ headers: { cookie: "wheel_session=bad" } }), "p1");
    expect(await readAll(res)).toContain('event: wheel-error\ndata: {"status":401}');
  });
});
