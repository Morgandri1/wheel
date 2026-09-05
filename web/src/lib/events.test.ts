import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The ticket rules, asserted.
 *
 * API: the ws-ticket is single-use and expires in 30 seconds, so it must be minted immediately
 * before the socket opens and freshly again on every reconnect. A socket that opens and then
 * closes is usually a replayed ticket rather than a network fault. That is a property a
 * perfectly reasonable refactor — hoisting the ticket up to where the board mounts, say — would
 * quietly break, and the symptom would look like flaky networking. So it is a test.
 */
// The ticket carries the project it was minted for, so a test can prove the socket used that one.
const wsTicket = vi.fn(async (projectId: string) => ({
  ticket: `${projectId}-t${wsTicket.mock.calls.length}`,
  expires_in: 30,
}));

vi.mock("@/lib/api", () => ({
  API_URL: "http://api.test",
  projects: { wsTicket: (id: string) => wsTicket(id) },
}));

class FakeSocket {
  static instances: FakeSocket[] = [];
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  closed = false;
  constructor(readonly url: string) {
    FakeSocket.instances.push(this);
  }
  close() {
    this.closed = true;
    this.onclose?.();
  }
}

const ticketOf = (url: string) => new URL(url).searchParams.get("ticket");
const settle = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  FakeSocket.instances = [];
  wsTicket.mockClear();
  vi.stubGlobal("WebSocket", FakeSocket);
  vi.stubGlobal("requestAnimationFrame", (cb: () => void) => setTimeout(cb, 0) as unknown as number);
  vi.stubGlobal("cancelAnimationFrame", (id: number) => clearTimeout(id));
  vi.useFakeTimers({ shouldAdvanceTime: true });
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.resetModules();
});

describe("the events socket", () => {
  it("mints the ticket immediately before opening, and puts it in the query", async () => {
    const { connectEvents } = await import("./events");
    const stop = connectEvents("p1", { onBatch: () => {}, onStatus: () => {} });
    await settle();

    expect(wsTicket).toHaveBeenCalledTimes(1);
    expect(FakeSocket.instances).toHaveLength(1);
    // Minted for THIS project, and used by the socket that was opened for it.
    expect(wsTicket).toHaveBeenCalledWith("p1");
    expect(ticketOf(FakeSocket.instances[0]!.url)).toBe("p1-t1");
    stop();
  });

  it("mints a FRESH ticket on every reconnect rather than replaying one", async () => {
    const { connectEvents } = await import("./events");
    const stop = connectEvents("p1", { onBatch: () => {}, onStatus: () => {} });
    await settle();

    FakeSocket.instances[0]!.close();
    await vi.advanceTimersByTimeAsync(1000);
    await settle();

    expect(FakeSocket.instances).toHaveLength(2);
    const [first, second] = FakeSocket.instances;
    expect(ticketOf(second!.url)).not.toBe(ticketOf(first!.url));
    expect(wsTicket).toHaveBeenCalledTimes(2);
    stop();
  });

  it("never puts the session token in the socket URL", async () => {
    const { connectEvents } = await import("./events");
    const stop = connectEvents("p1", { onBatch: () => {}, onStatus: () => {} });
    await settle();

    const url = FakeSocket.instances[0]!.url;
    expect(url.startsWith("ws://api.test/")).toBe(true);
    expect(new URL(url).searchParams.has("token")).toBe(false);
    expect(url).not.toMatch(/x-auth-token|Bearer|eyJ/);
    stop();
  });

  it("gives up on a 401 instead of hammering the API with dead sessions", async () => {
    wsTicket.mockRejectedValueOnce(Object.assign(new Error("unauthenticated"), { status: 401 }));
    const { connectEvents } = await import("./events");
    const seen: string[] = [];
    const stop = connectEvents("p1", { onBatch: () => {}, onStatus: (s) => seen.push(s) });
    await settle();
    await vi.advanceTimersByTimeAsync(30_000);

    expect(seen.at(-1)).toBe("closed");
    expect(FakeSocket.instances).toHaveLength(0);
    expect(wsTicket).toHaveBeenCalledTimes(1);
    stop();
  });

  it("retries when minting fails for any other reason", async () => {
    wsTicket.mockRejectedValueOnce(Object.assign(new Error("boom"), { status: 503 }));
    const { connectEvents } = await import("./events");
    const stop = connectEvents("p1", { onBatch: () => {}, onStatus: () => {} });
    await settle();
    await vi.advanceTimersByTimeAsync(1000);
    await settle();

    expect(wsTicket).toHaveBeenCalledTimes(2);
    expect(FakeSocket.instances).toHaveLength(1);
    stop();
  });

  it("stops minting once the board is closed", async () => {
    const { connectEvents } = await import("./events");
    const stop = connectEvents("p1", { onBatch: () => {}, onStatus: () => {} });
    await settle();
    stop();

    FakeSocket.instances[0]!.close();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(wsTicket).toHaveBeenCalledTimes(1);
  });

  it("batches frames into one callback per animation frame", async () => {
    const { connectEvents } = await import("./events");
    const batches: number[] = [];
    const stop = connectEvents("p1", { onBatch: (b) => batches.push(b.length), onStatus: () => {} });
    await settle();

    const sock = FakeSocket.instances[0]!;
    for (let i = 0; i < 50; i += 1) sock.onmessage?.({ data: JSON.stringify({ type: "log", i }) });
    await vi.advanceTimersByTimeAsync(20);

    expect(batches).toEqual([50]);
    stop();
  });
});
