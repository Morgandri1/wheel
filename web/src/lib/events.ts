"use client";

/**
 * One WebSocket per open board, to the API's proxy of the engine event stream.
 *
 * Browsers cannot set headers on a WebSocket handshake, and the Clerk token must never appear in
 * a URL. So it travels as a WebSocket subprotocol (a request header, not a query string):
 *   Sec-WebSocket-Protocol: wheel.v1, wheel.token.<jwt>
 * Flagged to PM as an open question for API — if they prefer a short-lived ticket endpoint,
 * only connect() changes.
 *
 * Frames are buffered and flushed once per animation frame, so a chatty agent cannot drive one
 * React commit per log line.
 */
import { getAuthToken } from "@/lib/auth";
import { API_URL } from "@/lib/api";
import type { EngineEvent } from "@/lib/schema";

export type ConnectionStatus = "connecting" | "open" | "reconnecting" | "closed";

interface Handlers {
  onBatch: (events: EngineEvent[]) => void;
  onStatus: (status: ConnectionStatus) => void;
}

const BACKOFF_MS = [500, 1000, 2000, 4000, 8000, 15000] as const;

export function connectEvents(projectId: string, handlers: Handlers): () => void {
  let ws: WebSocket | null = null;
  let closed = false;
  let attempt = 0;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;

  let queue: EngineEvent[] = [];
  let frame: number | null = null;

  const flush = () => {
    frame = null;
    if (!queue.length) return;
    const batch = queue;
    queue = [];
    handlers.onBatch(batch);
  };

  const enqueue = (e: EngineEvent) => {
    queue.push(e);
    // Guard against an unbounded burst while the tab is backgrounded and rAF is throttled.
    if (queue.length > 2000) queue.splice(0, queue.length - 2000);
    if (frame === null) frame = requestAnimationFrame(flush);
  };

  const wsUrl = () => {
    const base = API_URL.replace(/^http/, "ws");
    return `${base}/v1/projects/${projectId}/engine/v1/events`;
  };

  const open = async () => {
    if (closed) return;
    handlers.onStatus(attempt === 0 ? "connecting" : "reconnecting");

    let token: string;
    try {
      token = await getAuthToken();
    } catch {
      handlers.onStatus("closed");
      return;
    }
    if (closed) return;

    try {
      ws = new WebSocket(wsUrl(), ["wheel.v1", `wheel.token.${token}`]);
    } catch {
      scheduleRetry();
      return;
    }

    ws.onopen = () => {
      attempt = 0;
      handlers.onStatus("open");
    };

    ws.onmessage = (ev) => {
      try {
        enqueue(JSON.parse(ev.data as string) as EngineEvent);
      } catch {
        /* a frame we can't parse is a frame we ignore */
      }
    };

    ws.onerror = () => ws?.close();

    ws.onclose = () => {
      ws = null;
      if (closed) return;
      scheduleRetry();
    };
  };

  const scheduleRetry = () => {
    if (closed || retryTimer) return;
    const wait = BACKOFF_MS[Math.min(attempt, BACKOFF_MS.length - 1)]!;
    attempt++;
    handlers.onStatus("reconnecting");
    retryTimer = setTimeout(() => {
      retryTimer = null;
      void open();
    }, wait + Math.random() * 250);
  };

  void open();

  return () => {
    closed = true;
    if (retryTimer) clearTimeout(retryTimer);
    if (frame !== null) cancelAnimationFrame(frame);
    handlers.onStatus("closed");
    ws?.close();
    ws = null;
  };
}
