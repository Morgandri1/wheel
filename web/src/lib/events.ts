"use client";

/**
 * One WebSocket per open board, to the API's proxy of the engine event stream.
 *
 * Browsers cannot set headers on a WebSocket handshake, and the session JWT must never appear
 * in a URL. §5 answers both: mint a single-use ticket bound to (user, project) that expires in
 * 30 seconds, and put that in the query string instead. A fresh ticket is minted per attempt,
 * so a reconnect after a long backoff never replays a stale one.
 *
 * Frames are buffered and flushed once per animation frame, so a chatty agent cannot drive one
 * React commit per log line.
 */
import { API_URL, projects } from "@/lib/api";
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

  const wsUrl = (ticket: string) => {
    const base = API_URL.replace(/^http/, "ws");
    return `${base}/v1/projects/${projectId}/engine/v1/events?ticket=${encodeURIComponent(ticket)}`;
  };

  const open = async () => {
    if (closed) return;
    handlers.onStatus(attempt === 0 ? "connecting" : "reconnecting");

    let ticket: string;
    try {
      ({ ticket } = await projects.wsTicket(projectId));
    } catch (e) {
      // An expired session is terminal; anything else is worth another attempt.
      if ((e as { status?: number })?.status === 401) {
        handlers.onStatus("closed");
        return;
      }
      scheduleRetry();
      return;
    }
    if (closed) return;

    try {
      ws = new WebSocket(wsUrl(ticket));
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
