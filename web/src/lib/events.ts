"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * One EventSource per open board, to this app's own relay of the engine event stream.
 *
 * The relay (`/api/wheel/projects/:id/events`, `src/lib/event-relay.ts`) opens the engine socket
 * server-side with the session in a header, so the browser holds no ticket, no token and no API
 * address. EventSource shows the page no status code, so the relay names two events: `wheel-open`
 * once the upstream socket is really connected, and `wheel-error` carrying the upstream status.
 *
 * Reconnection is ours, not the browser's. EventSource's built-in retry has no backoff and no idea
 * that a 401 is final, so every error closes it and the schedule below decides what happens next.
 *
 * Frames are buffered and flushed once per animation frame, so a chatty agent cannot drive one
 * React commit per log line.
 */
import { notifyUnauthorized } from "@/lib/auth";
import type { EngineFrame } from "@/lib/schema";

export type ConnectionStatus = "connecting" | "open" | "reconnecting" | "closed";

interface Handlers {
  onBatch: (events: EngineFrame[]) => void;
  onStatus: (status: ConnectionStatus) => void;
}

const BACKOFF_MS = [500, 1000, 2000, 4000, 8000, 15000] as const;

export function eventsPath(projectId: string): string {
  return `/api/wheel/projects/${encodeURIComponent(projectId)}/events`;
}

function upstreamStatus(event: Event): number | null {
  try {
    const status = (JSON.parse((event as MessageEvent<string>).data) as { status?: unknown })?.status;
    return typeof status === "number" ? status : null;
  } catch {
    return null;
  }
}

export function connectEvents(projectId: string, handlers: Handlers): () => void {
  let source: EventSource | null = null;
  let closed = false;
  let attempt = 0;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;

  let queue: EngineFrame[] = [];
  let frame: number | null = null;

  const flush = () => {
    frame = null;
    if (!queue.length) return;
    const batch = queue;
    queue = [];
    handlers.onBatch(batch);
  };

  const enqueue = (e: EngineFrame) => {
    queue.push(e);
    // Guard against an unbounded burst while the tab is backgrounded and rAF is throttled.
    if (queue.length > 2000) queue.splice(0, queue.length - 2000);
    if (frame === null) frame = requestAnimationFrame(flush);
  };

  const drop = () => {
    source?.close();
    source = null;
  };

  const open = () => {
    if (closed) return;
    handlers.onStatus(attempt === 0 ? "connecting" : "reconnecting");

    let es: EventSource;
    try {
      es = new EventSource(eventsPath(projectId));
    } catch {
      scheduleRetry();
      return;
    }
    source = es;

    es.addEventListener("wheel-open", () => {
      attempt = 0;
      handlers.onStatus("open");
    });

    es.onmessage = (ev) => {
      try {
        enqueue(JSON.parse(ev.data as string) as EngineFrame);
      } catch {
        /* a frame we can't parse is a frame we ignore */
      }
    };

    es.addEventListener("wheel-error", (ev) => {
      drop();
      // An expired session is terminal; anything else is worth another attempt.
      if (upstreamStatus(ev) === 401) {
        notifyUnauthorized();
        handlers.onStatus("closed");
        return;
      }
      scheduleRetry();
    });

    es.onerror = () => {
      if (source !== es) return;
      drop();
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
      open();
    }, wait + Math.random() * 250);
  };

  open();

  return () => {
    closed = true;
    if (retryTimer) clearTimeout(retryTimer);
    if (frame !== null) cancelAnimationFrame(frame);
    handlers.onStatus("closed");
    drop();
  };
}
