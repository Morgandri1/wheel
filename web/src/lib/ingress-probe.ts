// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";
import { PROJECT_ID, errorEnvelope, readCapped, readUpTo } from "@/lib/proxy-rules";
import { serverAuthMode } from "@/lib/runtime-config";
import { refuseCrossOrigin } from "@/lib/same-origin";
import {
  apiUnreachable,
  apiUrl,
  callApi,
  clearCookieOn401,
  passThrough,
  unauthenticated,
  upstreamToken,
} from "@/lib/upstream";

/**
 * `POST /api/wheel/probe` — the endpoint panel's "test" button, run from this server.
 *
 * The browser used to hit the public URL itself; with `connect-src 'self'` it no longer can, and
 * it no longer knows the API's address anyway. So this server hits `${WHEEL_API_URL}/p/<id><path>`
 * — the same ingress handler the public URL reaches — and reports what came back.
 *
 * It asks the API whether the caller owns the project first. Ingress is public, but this server
 * reaching it on someone's behalf is not, least of all when the API itself is not public. The
 * ingress request itself carries no credential of any kind.
 */

const PROBE_METHODS = new Set(["GET", "POST", "PUT", "DELETE"]);
const REQUEST_LIMIT = 4 * 1024;
const ANSWER_LIMIT = 64 * 1024;

/** An endpoint's configured path, percent-encoded segment by segment, or null if it could escape `/p/<id>`. */
export function ingressPath(path: unknown): string | null {
  if (typeof path !== "string" || !path.startsWith("/") || path.length > 1024) return null;
  const encoded: string[] = [];
  for (const segment of path.slice(1).split("/")) {
    if (segment === "." || segment === ".." || /[\\\u0000-\u001f\u007f]/.test(segment)) return null;
    encoded.push(encodeURIComponent(segment));
  }
  return `/${encoded.join("/")}`;
}

export async function probeIngress(req: Request): Promise<Response> {
  const refused = refuseCrossOrigin(req);
  if (refused) return refused;

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

  const owned = await callApi(apiUrl(`/v1/projects/${projectId}`), { method: "GET", token, projectId });
  if (!owned) return apiUnreachable();
  if (!owned.ok) return passThrough(owned, clearCookieOn401(owned.status, req, mode));
  await owned.body?.cancel();

  const hit = await callApi(
    apiUrl(`/p/${projectId}${path}`),
    method === "GET" ? { method } : { method, json: { source: "wheel-endpoint-test" } },
  );
  if (!hit) return apiUnreachable();
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
