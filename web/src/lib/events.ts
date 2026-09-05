/**
 * One WebSocket per open board, to /v1/projects/:id/engine/v1/events.
 *
 * Frames arrive faster than React should re-render, so everything is buffered
 * and flushed once per animation frame. Reconnect is exponential with jitter and
 * a visible state, because a board that has silently stopped updating is worse
 * than one that says it is offline.
 *
 * Token transport: browsers cannot set headers on a WebSocket, and §"never put
 * the token in a URL" rules out a query string, so the token rides as a
 * subprotocol — `bearer.<token>`. API must accept this (see plans/web.md §6 Q7).
 */
import type { EngineEvent } from "@/lib/schema";

export type ConnectionState = "connecting" | "open" | "reconnecting" | "offline";

export interface EventClientOptions {
  url: string;
  getToken: () => Promise<string | null>;
  onBatch: (events: EngineEvent[]) => void;
  onState: (state: ConnectionState) => void;
}

const MAX_BACKOFF_MS = 15_000;
const BASE_BACKOFF_MS = 400;

export class EventClient {
  private socket: WebSocket | null = null;
  private buffer: EngineEvent[] = [];
  private frame: number | null = null;
  private attempt = 0;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private closed = false;

  constructor(private readonly options: EventClientOptions) {}

  async connect(): Promise<void> {
    if (this.closed) return;
    this.options.onState(this.attempt === 0 ? "connecting" : "reconnecting");

    let token: string | null = null;
    try {
      token = await this.options.getToken();
    } catch {
      this.scheduleRetry();
      return;
    }
    if (this.closed) return;

    const protocols = ["wheel.v1"];
    if (token) protocols.push(`bearer.${token}`);

    let socket: WebSocket;
    try {
      socket = new WebSocket(this.options.url, protocols);
    } catch {
      this.scheduleRetry();
      return;
    }
    this.socket = socket;

    socket.onopen = () => {
      this.attempt = 0;
      this.options.onState("open");
    };

    socket.onmessage = (event) => {
      if (typeof event.data !== "string") return;
      try {
        const parsed = JSON.parse(event.data) as EngineEvent;
        this.buffer.push(parsed);
        this.scheduleFlush();
      } catch {
        // A frame we cannot parse is the engine's problem, not a reason to drop
        // the connection. Ignore it and keep the stream alive.
      }
    };

    socket.onerror = () => socket.close();

    socket.onclose = () => {
      this.socket = null;
      if (!this.closed) this.scheduleRetry();
    };
  }

  private scheduleFlush() {
    if (this.frame !== null) return;
    const run = () => {
      this.frame = null;
      if (this.buffer.length === 0) return;
      const batch = this.buffer;
      this.buffer = [];
      this.options.onBatch(batch);
    };
    this.frame =
      typeof requestAnimationFrame === "function"
        ? requestAnimationFrame(run)
        : (setTimeout(run, 16) as unknown as number);
  }

  private scheduleRetry() {
    if (this.closed || this.retryTimer) return;
    this.attempt += 1;
    this.options.onState(this.attempt > 4 ? "offline" : "reconnecting");
    const capped = Math.min(BASE_BACKOFF_MS * 2 ** (this.attempt - 1), MAX_BACKOFF_MS);
    const delay = capped / 2 + Math.random() * (capped / 2);
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      void this.connect();
    }, delay);
  }

  /** Force an immediate attempt — the "Reconnect" button in the status chip. */
  retryNow() {
    if (this.retryTimer) {
      clearTimeout(this.retryTimer);
      this.retryTimer = null;
    }
    this.attempt = 0;
    this.socket?.close();
    void this.connect();
  }

  close() {
    this.closed = true;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    if (this.frame !== null && typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(this.frame);
    }
    this.socket?.close();
    this.socket = null;
  }
}
