import "server-only";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * The Workflow Builder's turn, relayed from the API to the browser.
 *
 * Its own route rather than the generic proxy for two reasons the proxy cannot give it: a builder
 * turn streams for as long as the model takes (the engine caps it at 240s), and a stream held open
 * is a resource this server has to bound per session the way `event-relay.ts` bounds board events.
 *
 * The session is attached here and never reaches the browser. The body is small and read whole —
 * a conversation, not an upload — so it can be size-checked before anything upstream is touched.
 */
import { errorEnvelope, PROJECT_ID, readCapped } from "@/lib/proxy-rules";
import { proxyBodyLimit, serverApiBaseUrl, serverAuthMode } from "@/lib/runtime-config";
import { refuseCrossOrigin } from "@/lib/same-origin";
import { StreamSlots } from "@/lib/event-relay";
import { answered, apiFailed, callApi, clearCookieOn401, upstreamToken } from "@/lib/upstream";

/** The engine kills a turn at 240s; this is that plus the hops, so its own `timeout` frame wins. */
const TURN_TIMEOUT_MS = 300_000;

export const BUILDER_SSE_HEADERS: Record<string, string> = {
  "content-type": "text/event-stream; charset=utf-8",
  "cache-control": "no-cache, no-transform",
  "x-accel-buffering": "no",
};

/**
 * Fewer than the board-event limit: a builder turn is a deliberate act that costs the user money,
 * so a handful in flight at once is already more than a person can be having.
 */
const slots = new StreamSlots({ perSession: 2, perAddress: 8, total: 64 });

export async function relayBuilderTurn(
  req: Request,
  projectId: string,
  streamSlots: StreamSlots = slots,
): Promise<Response> {
  const refused = refuseCrossOrigin(req);
  if (refused) return refused;
  if (!PROJECT_ID.test(projectId)) return errorEnvelope(404, "not_found", "There is no such project.");

  const mode = serverAuthMode();
  const token = await upstreamToken(req, mode);
  if (!token) {
    return errorEnvelope(401, "unauthenticated", "You're signed out. Sign in again.", clearCookieOn401(401, req, mode));
  }

  const limit = proxyBodyLimit();
  const body = await readCapped(req.body, limit);
  if (body === null) {
    return errorEnvelope(413, "payload_too_large", `Request bodies over ${limit} bytes are refused.`);
  }

  const release = streamSlots.acquire(token, null);
  if (!release) {
    return errorEnvelope(429, "too_many_streams", "Too many builder turns are already running for this session.");
  }

  const target = `${serverApiBaseUrl()}/v1/projects/${encodeURIComponent(projectId)}/builder/turns`;
  const result = await callApi(target, {
    method: "POST",
    token,
    projectId,
    body,
    headers: new Headers({ "content-type": "application/json" }),
    signal: req.signal,
    timeoutMs: TURN_TIMEOUT_MS,
  });

  if (!answered(result)) {
    release();
    return apiFailed(result);
  }

  // A refusal that arrived instead of a stream — no credential, policy, busy — is an ordinary JSON
  // answer and is handed back as one, status and all.
  const upstreamType = result.headers.get("content-type") ?? "";
  if (!result.ok || !upstreamType.includes("text/event-stream") || !result.body) {
    release();
    const text = await result.text().catch(() => "");
    return new Response(text || null, {
      status: result.status,
      headers: {
        "content-type": upstreamType.includes("json") ? "application/json" : "application/json",
        "x-content-type-options": "nosniff",
      },
    });
  }

  // The slot is held for exactly as long as the stream is, however it ends: read to the end,
  // cancelled by the reader, or torn down with the request.
  //
  // Pull-based on purpose. Draining the upstream as fast as it arrives would ignore the reader's
  // backpressure and buffer the answer here instead of streaming it — and would free the slot the
  // moment the upstream finished rather than when the browser was actually done with it.
  const reader = result.body.getReader();
  let released = false;
  const finish = () => {
    if (released) return;
    released = true;
    release();
  };
  const abort = () => {
    finish();
    reader.cancel().catch(() => {});
  };
  req.signal.addEventListener("abort", abort);

  const stream = new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        const { done, value } = await reader.read();
        if (done) {
          req.signal.removeEventListener("abort", abort);
          finish();
          controller.close();
          return;
        }
        controller.enqueue(value);
      } catch {
        // The API hung up mid-answer. The client sees the stream end without a terminal frame,
        // which `builder-stream.ts` reports as a broken stream rather than a finished answer.
        req.signal.removeEventListener("abort", abort);
        finish();
        controller.close();
      }
    },
    cancel() {
      req.signal.removeEventListener("abort", abort);
      abort();
    },
  });

  return new Response(stream, { headers: { ...BUILDER_SSE_HEADERS, "x-content-type-options": "nosniff" } });
}
