// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The relay's rules, asserted: one EventSource at this app's own path, `open` only when the relay
 * says upstream is live, our own backoff instead of the browser's, and a 401 that ends the session
 * rather than retrying forever. A refactor that restored EventSource's native reconnect, or marked
 * the board live when the HTTP response arrived, would look fine and behave wrongly.
 */
type Listener = (e: { data?: string }) => void;

class FakeEventSource {
  static instances: FakeEventSource[] = [];
  static throwNext = false;
  onmessage: ((e: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  closed = false;
  private readonly listeners = new Map<string, Listener[]>();

  constructor(readonly url: string) {
    if (FakeEventSource.throwNext) {
      FakeEventSource.throwNext = false;
      throw new SyntaxError("bad url");
    }
    FakeEventSource.instances.push(this);
  }

  addEventListener(type: string, fn: Listener) {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), fn]);
  }

  emit(type: string, data?: string) {
    for (const fn of this.listeners.get(type) ?? []) fn({ data });
  }

  message(frame: unknown) {
    this.onmessage?.({ data: typeof frame === "string" ? frame : JSON.stringify(frame) });
  }

  close() {
    this.closed = true;
  }
}

const sources = () => FakeEventSource.instances;
const latest = () => sources().at(-1)!;

async function load() {
  vi.resetModules();
  const events = await import("./events");
  const auth = await import("./auth");
  return { ...events, auth };
}

function watch() {
  const statuses: string[] = [];
  const batches: unknown[][] = [];
  return { statuses, batches, handlers: { onStatus: (s: string) => statuses.push(s), onBatch: (b: unknown[]) => batches.push(b) } };
}

beforeEach(() => {
  FakeEventSource.instances = [];
  FakeEventSource.throwNext = false;
  vi.useFakeTimers();
  vi.stubGlobal("EventSource", FakeEventSource);
  vi.stubGlobal("requestAnimationFrame", (cb: () => void) => setTimeout(cb, 0) as unknown as number);
  vi.stubGlobal("cancelAnimationFrame", (id: number) => clearTimeout(id));
  vi.spyOn(Math, "random").mockReturnValue(0);
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("where it connects", () => {
  it("opens one EventSource, at this app's own relay for the project", async () => {
    const { connectEvents } = await load();
    const stop = connectEvents("p1", watch().handlers);
    expect(sources()).toHaveLength(1);
    expect(latest().url).toBe("/api/wheel/projects/p1/events");
    stop();
  });

  it("names no API origin, ticket or token", async () => {
    const { connectEvents } = await load();
    const stop = connectEvents("9b1d-44", watch().handlers);
    expect(latest().url.startsWith("/")).toBe(true);
    expect(latest().url).not.toMatch(/\/\/|\?|ticket|token/);
    stop();
  });

  it("escapes the project id into a single path segment", async () => {
    const { eventsPath } = await load();
    expect(eventsPath("a/../b")).toBe("/api/wheel/projects/a%2F..%2Fb/events");
  });
});

describe("status", () => {
  it("stays connecting until the relay says upstream is live, not merely when the response arrives", async () => {
    const { connectEvents } = await load();
    const w = watch();
    const stop = connectEvents("p1", w.handlers);
    expect(w.statuses).toEqual(["connecting"]);
    latest().emit("open");
    expect(w.statuses).toEqual(["connecting"]);
    latest().emit("wheel-open");
    expect(w.statuses).toEqual(["connecting", "open"]);
    stop();
  });
});

describe("frames", () => {
  it("batches frames into one callback per animation frame", async () => {
    const { connectEvents } = await load();
    const w = watch();
    const stop = connectEvents("p1", w.handlers);
    for (let i = 0; i < 50; i += 1) latest().message({ type: "log", i });
    vi.advanceTimersByTime(16);
    expect(w.batches.map((b) => b.length)).toEqual([50]);
    stop();
  });

  it("ignores a frame it cannot parse", async () => {
    const { connectEvents } = await load();
    const w = watch();
    const stop = connectEvents("p1", w.handlers);
    latest().message("{{{");
    latest().message({ type: "log" });
    vi.advanceTimersByTime(16);
    expect(w.batches).toEqual([[{ type: "log" }]]);
    stop();
  });

  it("keeps only the newest 2000 frames while a background tab throttles animation frames", async () => {
    const { connectEvents } = await load();
    const w = watch();
    const stop = connectEvents("p1", w.handlers);
    for (let i = 0; i < 2100; i += 1) latest().message({ i });
    vi.advanceTimersByTime(16);
    expect(w.batches[0]).toHaveLength(2000);
    expect(w.batches[0]![0]).toEqual({ i: 100 });
    stop();
  });
});

describe("reconnecting", () => {
  it("closes a failed EventSource itself and retries on our schedule, not the browser's", async () => {
    const { connectEvents } = await load();
    const w = watch();
    const stop = connectEvents("p1", w.handlers);
    latest().onerror!();
    expect(sources()[0]!.closed).toBe(true);
    expect(w.statuses.at(-1)).toBe("reconnecting");
    vi.advanceTimersByTime(499);
    expect(sources()).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(sources()).toHaveLength(2);
    stop();
  });

  it("waits longer after each consecutive failure", async () => {
    const { connectEvents } = await load();
    const stop = connectEvents("p1", watch().handlers);
    latest().onerror!();
    vi.advanceTimersByTime(500);
    latest().onerror!();
    vi.advanceTimersByTime(999);
    expect(sources()).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(sources()).toHaveLength(3);
    stop();
  });

  it("starts the schedule over once a connection has really opened", async () => {
    const { connectEvents } = await load();
    const stop = connectEvents("p1", watch().handlers);
    latest().onerror!();
    vi.advanceTimersByTime(500);
    latest().emit("wheel-open");
    latest().onerror!();
    vi.advanceTimersByTime(500);
    expect(sources()).toHaveLength(3);
    stop();
  });

  it.each([
    ["an upstream failure", '{"status":502}'],
    ["an unreadable error", "not json"],
    ["an error with no status", "{}"],
  ])("retries after %s from the relay", async (_label, data) => {
    const { connectEvents } = await load();
    const stop = connectEvents("p1", watch().handlers);
    latest().emit("wheel-error", data);
    expect(sources()[0]!.closed).toBe(true);
    vi.advanceTimersByTime(500);
    expect(sources()).toHaveLength(2);
    stop();
  });

  it("ignores an error from a source it has already replaced", async () => {
    const { connectEvents } = await load();
    const stop = connectEvents("p1", watch().handlers);
    const first = latest();
    first.emit("wheel-error", '{"status":502}');
    first.onerror!();
    vi.advanceTimersByTime(5000);
    expect(sources()).toHaveLength(2);
    stop();
  });

  it("retries when EventSource cannot even be constructed", async () => {
    const { connectEvents } = await load();
    FakeEventSource.throwNext = true;
    const stop = connectEvents("p1", watch().handlers);
    expect(sources()).toHaveLength(0);
    vi.advanceTimersByTime(500);
    expect(sources()).toHaveLength(1);
    stop();
  });
});

describe("a dead session", () => {
  it("ends the session instead of retrying forever", async () => {
    const { connectEvents, auth } = await load();
    const unauthorized = vi.fn();
    auth.setUnauthorizedHandler(unauthorized);
    const w = watch();
    const stop = connectEvents("p1", w.handlers);
    latest().emit("wheel-error", '{"status":401}');
    vi.advanceTimersByTime(60_000);
    expect(unauthorized).toHaveBeenCalledOnce();
    expect(w.statuses.at(-1)).toBe("closed");
    expect(sources()).toHaveLength(1);
    expect(sources()[0]!.closed).toBe(true);
    stop();
  });
});

describe("stopping", () => {
  it("closes the source, reports closed, and never reconnects", async () => {
    const { connectEvents } = await load();
    const w = watch();
    const stop = connectEvents("p1", w.handlers);
    const first = latest();
    stop();
    expect(first.closed).toBe(true);
    expect(w.statuses.at(-1)).toBe("closed");
    first.onerror?.();
    vi.advanceTimersByTime(30_000);
    expect(sources()).toHaveLength(1);
  });

  it("cancels a pending retry", async () => {
    const { connectEvents } = await load();
    const stop = connectEvents("p1", watch().handlers);
    latest().onerror!();
    stop();
    vi.advanceTimersByTime(30_000);
    expect(sources()).toHaveLength(1);
  });

  it("drops frames still waiting for an animation frame", async () => {
    const { connectEvents } = await load();
    const w = watch();
    const stop = connectEvents("p1", w.handlers);
    latest().message({ type: "log" });
    stop();
    vi.advanceTimersByTime(16);
    expect(w.batches).toEqual([]);
  });
});
