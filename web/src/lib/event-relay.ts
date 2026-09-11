// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";
import WebSocket from "ws";
import { PROJECT_ID, errorEnvelope } from "@/lib/proxy-rules";
import { serverApiBaseUrl } from "@/lib/runtime-config";
import { refuseCrossOrigin } from "@/lib/same-origin";
import { upstreamToken } from "@/lib/upstream";

/**
 * The engine's event socket, relayed to the browser as Server-Sent Events.
 *
 * This server opens `${WHEEL_API_URL}/v1/projects/:id/engine/v1/events` with the session in the
 * `x-auth-token` HEADER — the form the API accepts from non-browser clients — so no ticket is
 * minted and no credential ever sits in a URL. Each upstream frame becomes one `data:` event,
 * verbatim: the `message` event must reach the UI byte-for-byte so it can correlate rows by id.
 *
 * EventSource shows the page no status code, so two named events carry what it cannot:
 * `wheel-open` once the upstream socket is really connected, and `wheel-error` with the upstream
 * status when it is not, so a 401 can end the session instead of retrying forever.
 */

export const HEARTBEAT_MS = 15_000;
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

export async function relayEvents(req: Request, projectId: string, connect: Connect = openSocket): Promise<Response> {
  const refused = refuseCrossOrigin(req);
  if (refused) return refused;
  if (!PROJECT_ID.test(projectId)) return errorEnvelope(404, "not_found", "There is no such project.");

  const token = await upstreamToken(req);
  const url = eventsSocketUrl(serverApiBaseUrl(), projectId);
  const encoder = new TextEncoder();
  let stop = () => {};

  const stream = new ReadableStream<Uint8Array>(
    {
      start(controller) {
        let finished = false;
        let socket: UpstreamSocket | null = null;
        let heartbeat: ReturnType<typeof setInterval> | undefined = undefined;

        const finish = () => {
          if (finished) return;
          finished = true;
          clearInterval(heartbeat);
          req.signal.removeEventListener("abort", finish);
          socket?.terminate();
          try {
            controller.close();
          } catch {
            /* the reader already cancelled */
          }
        };
        const send = (text: string) => {
          if (finished) return;
          controller.enqueue(encoder.encode(text));
          // A reader this far behind is not catching up. Closing makes it reconnect and backfill.
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
      },
      cancel() {
        stop();
      },
    },
    new ByteLengthQueuingStrategy({ highWaterMark: MAX_QUEUED_BYTES }),
  );

  return new Response(stream, { headers: SSE_HEADERS });
}
