// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";
import WebSocket from "ws";
import { PROJECT_ID, errorEnvelope } from "@/lib/proxy-rules";
import { serverApiBaseUrl, serverAuthMode } from "@/lib/runtime-config";
import { clientAddress, refuseCrossOrigin } from "@/lib/same-origin";
import { upstreamToken } from "@/lib/upstream";

/**
 * The engine's event socket, relayed to the browser as Server-Sent Events.
 *
 * This server opens `${WHEEL_API_URL}/v1/projects/:id/engine/v1/events` with the session in the
 * `x-auth-token` HEADER, so no ticket is minted and no credential ever sits in a URL. Each upstream
 * frame becomes one `data:` event, verbatim: the `message` event must reach the UI byte for byte.
 *
 * EventSource shows the page no status code, so two named events carry what it cannot:
 * `wheel-open` once the upstream socket is really connected, and `wheel-error` with the upstream
 * status when it is not, so a 401 can end the session instead of retrying forever.
 *
 * The relay has no replay. Whatever a stream misses while it is down — the reader fell behind, the
 * platform cut it, it hit its lifetime — is gone, and the board refetches when the stream reopens
 * (`onResync` in events.ts).
 */

export const HEARTBEAT_MS = 15_000;
/** A stream is re-established this often, so a session revoked at the API stops streaming within it. */
export const MAX_STREAM_MS = 15 * 60_000;
const HANDSHAKE_TIMEOUT_MS = 10_000;
const MAX_FRAME_BYTES = 16 * 1024 * 1024;
const MAX_QUEUED_BYTES = 1024 * 1024;

export const SSE_HEADERS: Record<string, string> = {
  "content-type": "text/event-stream; charset=utf-8",
  // no-transform keeps compression middleware from buffering the stream into one late response.
  "cache-control": "no-cache, no-transform",
  "x-accel-buffering": "no",
};
export const SSE_HEARTBEAT = ": keepalive\n\n";

export function sseData(frame: string): string {
  return `${frame
    .split(/\r\n|\r|\n/)
    .map((line) => `data: ${line}`)
    .join("\n")}\n\n`;
}

export function sseEvent(name: "wheel-open" | "wheel-error", data: object): string {
  return `event: ${name}\n${sseData(JSON.stringify(data))}`;
}

export function eventsSocketUrl(apiBase: string, projectId: string): string {
  return `${apiBase.replace(/^http/, "ws")}/v1/projects/${projectId}/engine/v1/events`;
}

/**
 * Concurrent streams held open, capped per session, per client address and in total, so neither a
 * cookie nor an address can hold an unbounded number of upstream sockets. The address is only as
 * good as the proxy that recorded it (`clientAddress`); the total is the backstop either way.
 */
export class StreamSlots {
  private readonly held = new Map<string, number>();

  constructor(private readonly limits: { perSession: number; perAddress: number; total: number }) {}

  /** A release function, or null when any limit is already reached. */
  acquire(session: string | null, address: string | null): (() => void) | null {
    const keys: [string, number][] = [["total", this.limits.total]];
    if (session) keys.push([`session:${session}`, this.limits.perSession]);
    if (address) keys.push([`address:${address}`, this.limits.perAddress]);
    if (keys.some(([key, max]) => (this.held.get(key) ?? 0) >= max)) return null;
    for (const [key] of keys) this.held.set(key, (this.held.get(key) ?? 0) + 1);
    let released = false;
    return () => {
      if (released) return;
      released = true;
      for (const [key] of keys) {
        const left = (this.held.get(key) ?? 1) - 1;
        if (left > 0) this.held.set(key, left);
        else this.held.delete(key);
      }
    };
  }

  get open(): number {
    return this.held.get("total") ?? 0;
  }
}

const slots = new StreamSlots({ perSession: 8, perAddress: 32, total: 1024 });

export interface UpstreamSocket {
  on(event: string, listener: (...args: never[]) => void): unknown;
  terminate(): void;
}

export type Connect = (url: string, headers: Record<string, string>) => UpstreamSocket;

const openSocket: Connect = (url, headers) =>
  new WebSocket(url, {
    headers,
    handshakeTimeout: HANDSHAKE_TIMEOUT_MS,
    maxPayload: MAX_FRAME_BYTES,
    followRedirects: false,
  });

function frameText(data: WebSocket.RawData): string {
  if (Array.isArray(data)) return Buffer.concat(data).toString("utf8");
  return (Buffer.isBuffer(data) ? data : Buffer.from(data)).toString("utf8");
}

export async function relayEvents(
  req: Request,
  projectId: string,
  connect: Connect = openSocket,
  streamSlots: StreamSlots = slots,
): Promise<Response> {
  const refused = refuseCrossOrigin(req);
  if (refused) return refused;
  if (!PROJECT_ID.test(projectId)) return errorEnvelope(404, "not_found", "There is no such project.");

  const mode = serverAuthMode();
  const token = await upstreamToken(req, mode);
  let release: (() => void) | null = () => {};
  if (token) {
    release = streamSlots.acquire(mode === "local" ? token : null, clientAddress(req));
    if (!release) {
      return errorEnvelope(429, "too_many_streams", "Too many live boards are open for this session or address.");
    }
  }
  const releaseSlot = release;
  const url = eventsSocketUrl(serverApiBaseUrl(), projectId);
  const encoder = new TextEncoder();
  let stop = () => {};

  const stream = new ReadableStream<Uint8Array>(
    {
      start(controller) {
        let finished = false;
        let socket: UpstreamSocket | null = null;
        let heartbeat: ReturnType<typeof setInterval> | undefined = undefined;
        let lifetime: ReturnType<typeof setTimeout> | undefined = undefined;

        const finish = () => {
          if (finished) return;
          finished = true;
          clearInterval(heartbeat);
          clearTimeout(lifetime);
          req.signal.removeEventListener("abort", finish);
          socket?.terminate();
          releaseSlot();
          try {
            controller.close();
          } catch {
            /* the reader already cancelled */
          }
        };
        const send = (text: string) => {
          if (finished) return;
          controller.enqueue(encoder.encode(text));
          // A reader this far behind is not catching up; closing makes it reconnect and refetch.
          if ((controller.desiredSize ?? 0) < 0) finish();
        };
        const fail = (status: number) => {
          send(sseEvent("wheel-error", { status }));
          finish();
        };

        stop = finish;
        if (req.signal.aborted) return finish();
        req.signal.addEventListener("abort", finish);
        if (!token) return fail(401);

        send(SSE_HEARTBEAT);
        let opened = false;
        try {
          socket = connect(url, { "x-auth-token": token, "x-project-id": projectId });
        } catch {
          return fail(502);
        }
        socket.on("open", () => {
          opened = true;
          send(sseEvent("wheel-open", {}));
        });
        socket.on("message", (data: WebSocket.RawData) => send(sseData(frameText(data))));
        socket.on("unexpected-response", (_request: unknown, response: { statusCode?: number }) =>
          fail(response.statusCode ?? 502),
        );
        socket.on("error", () => (opened ? finish() : fail(502)));
        socket.on("close", finish);
        heartbeat = setInterval(() => send(SSE_HEARTBEAT), HEARTBEAT_MS);
        lifetime = setTimeout(finish, MAX_STREAM_MS);
      },
      cancel() {
        stop();
      },
    },
    new ByteLengthQueuingStrategy({ highWaterMark: MAX_QUEUED_BYTES }),
  );

  return new Response(stream, { headers: SSE_HEADERS });
}
