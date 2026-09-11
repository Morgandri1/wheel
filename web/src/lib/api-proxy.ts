// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";
import {
  apiTarget,
  cappedStream,
  declaredLength,
  errorEnvelope,
  forwardedRequestHeaders,
  upstreamPath,
} from "@/lib/proxy-rules";
import { proxyBodyLimit, serverApiBaseUrl, serverAuthMode } from "@/lib/runtime-config";
import { refuseCrossOrigin } from "@/lib/same-origin";
import { answered, apiFailed, callApi, clearCookieOn401, passThrough, unauthenticated, upstreamToken } from "@/lib/upstream";

/**
 * `/api/wheel/v1/projects…` → `${WHEEL_API_URL}/v1/projects…`, with the session attached here.
 *
 * Order matters and is the same on every request: refuse cross-origin, refuse a path or header
 * the rules will not pass, resolve the caller's credential, then stream the body through a
 * counter — never buffered — to the API.
 */
export async function proxyToApi(req: Request): Promise<Response> {
  const refused = refuseCrossOrigin(req);
  if (refused) return refused;

  const url = new URL(req.url);
  const path = upstreamPath(url.pathname);
  const target = path && apiTarget(serverApiBaseUrl(), path, url.search);
  if (!target) return errorEnvelope(404, "not_proxied", "This app does not forward that path to the API.");

  const headers = forwardedRequestHeaders(req.headers);
  if (!headers) {
    return errorEnvelope(400, "bad_header", "x-project-id or content-type has a value this app will not forward.");
  }

  const mode = serverAuthMode();
  const token = await upstreamToken(req, mode);
  if (!token) return unauthenticated(req, mode);

  const limit = proxyBodyLimit();
  let body: ReadableStream<Uint8Array> | undefined;
  let capped: ReturnType<typeof cappedStream> | undefined;
  if (req.body && req.method !== "GET" && req.method !== "HEAD") {
    if (declaredLength(req.headers) > limit) return tooLarge(limit);
    capped = cappedStream(limit);
    body = req.body.pipeThrough(capped.transform);
  }

  const res = await callApi(target, { method: req.method, token, headers, body, signal: req.signal });
  if (capped?.exceeded()) {
    if (answered(res)) await res.body?.cancel();
    return tooLarge(limit);
  }
  if (!answered(res)) return apiFailed(res);
  return passThrough(res, clearCookieOn401(res.status, req, mode));
}

function tooLarge(limit: number): Response {
  return errorEnvelope(413, "payload_too_large", `Request bodies over ${limit} bytes are refused.`);
}
