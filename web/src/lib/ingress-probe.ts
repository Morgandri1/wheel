// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";
import { PROJECT_ID, errorEnvelope, isJsonMediaType, readCapped, readUpTo } from "@/lib/proxy-rules";
import { serverAuthMode } from "@/lib/runtime-config";
import { refuseCrossOrigin } from "@/lib/same-origin";
import {
  API_CALL_TIMEOUT_MS,
  answered,
  apiFailed,
  apiUrl,
  callApi,
  clearCookieOn401,
  passThrough,
  unauthenticated,
  upstreamToken,
} from "@/lib/upstream";

/**
 * `POST /api/wheel/probe` — the endpoint panel's "send test", run from this server.
 *
 * The browser can neither reach nor name the API, so this server hits `${WHEEL_API_URL}/p/<id><path>`
 * — the ingress handler the public URL reaches — and reports what came back. It asks the API
 * whether the caller owns the project first: ingress is public, but this server reaching it on
 * someone's behalf is not, least of all when the API itself is not public. The ingress request
 * carries no credential of any kind.
 *
 * TWO DIFFERENT DEADLINES. The ownership check is a small, fast lookup — API_CALL_TIMEOUT_MS (5s)
 * is right for it, and a slow answer there really is "the API is unreachable". The ingress HIT is
 * not: a `script` endpoint runs its script before it answers (default 60s, ceiling 300s), so a real
 * hit that is doing real work is indistinguishable from a dead one at 5s. Holding it to the same
 * deadline reported delivered, running work as "the test did not run" — wrong on both counts: it WAS
 * sent, and the absence of an answer yet says nothing about whether the endpoint is down.
 *
 * HIT_TIMEOUT_MS gives it room without hanging the panel for the full five minutes: past it, this
 * reports SENT rather than failed, and the panel says so rather than "did not run".
 */

const PROBE_METHODS = new Set(["GET", "POST", "PUT", "DELETE"]);
const REQUEST_LIMIT = 4 * 1024;
const ANSWER_LIMIT = 64 * 1024;
/** Well under the 300s script ceiling, and well under a UI anyone would wait out without feedback. */
export const HIT_TIMEOUT_MS = 30_000;

/**
 * An endpoint's configured path, percent-encoded segment by segment, or null if it could escape
 * `/p/<id>`. A literal `%` is refused too: encoded once more it would be a second layer of
 * encoding for any hop that decodes twice.
 */
export function ingressPath(path: unknown): string | null {
  if (typeof path !== "string" || !path.startsWith("/") || path.length > 1024) return null;
  const encoded: string[] = [];
  for (const segment of path.slice(1).split("/")) {
    if (segment === "." || segment === ".." || /[%\\\u0000-\u001f\u007f]/.test(segment)) return null;
    encoded.push(encodeURIComponent(segment));
  }
  return `/${encoded.join("/")}`;
}

export async function probeIngress(req: Request): Promise<Response> {
  const refused = refuseCrossOrigin(req);
  if (refused) return refused;
  if (!isJsonMediaType(req.headers.get("content-type"))) {
    return errorEnvelope(415, "unsupported_media_type", "Send the probe as application/json.");
  }

  const input = await readObject(req);
  const projectId = input?.project_id;
  const method = input?.method;
  const path = ingressPath(input?.path);
  if (typeof projectId !== "string" || !PROJECT_ID.test(projectId) || typeof method !== "string" || !PROBE_METHODS.has(method) || !path) {
    return errorEnvelope(400, "bad_probe", "A probe needs a project id, one of GET, POST, PUT or DELETE, and an endpoint path.");
  }

  const mode = serverAuthMode();
  const token = await upstreamToken(req, mode);
  if (!token) return unauthenticated(req, mode);

  const owned = await callApi(apiUrl(`/v1/projects/${projectId}`), {
    method: "GET",
    token,
    projectId,
    timeoutMs: API_CALL_TIMEOUT_MS,
  });
  if (!answered(owned)) return apiFailed(owned);
  if (!owned.ok) return passThrough(owned, clearCookieOn401(owned.status, req, mode));
  await owned.body?.cancel();

  const hit = await callApi(apiUrl(`/p/${projectId}${path}`), {
    method,
    timeoutMs: HIT_TIMEOUT_MS,
    ...(method === "GET" ? {} : { json: { source: "wheel-endpoint-test" } }),
  });
  if (!answered(hit)) {
    // A timeout here is not a failure to report: the hit was delivered and may still be running.
    // Anything else (the API itself unreachable) really is a failure — apiFailed, as before.
    if (hit.failure === "timeout") {
      return Response.json(
        { sent: true, timeout_ms: HIT_TIMEOUT_MS },
        { headers: { "cache-control": "no-store" } },
      );
    }
    return apiFailed(hit);
  }
  const { bytes, truncated } = await readUpTo(hit.body, ANSWER_LIMIT);
  return Response.json(
    { status: hit.status, status_text: hit.statusText, body: new TextDecoder().decode(bytes), truncated },
    { headers: { "cache-control": "no-store" } },
  );
}

async function readObject(req: Request): Promise<Record<string, unknown> | null> {
  const bytes = await readCapped(req.body, REQUEST_LIMIT);
  if (!bytes) return null;
  try {
    const parsed = JSON.parse(new TextDecoder().decode(bytes)) as unknown;
    return parsed && typeof parsed === "object" ? (parsed as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}
