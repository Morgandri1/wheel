// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";
import { errorEnvelope, isJsonMediaType, readCapped } from "@/lib/proxy-rules";
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
 * `POST /v1/invites/accept` is not project-scoped, so it is never reached through the generic
 * board proxy (`upstreamPath` only forwards `/v1/projects/…`) — same reason `/v1/auth/*` gets its
 * own routes in `session-routes.ts` rather than going through it. This is that route's sibling:
 * an authenticated call under whichever auth mode is configured, forwarded and passed through
 * unchanged, so the redeem page reads the API's own words for "this invite is not valid."
 */

const TOKEN_BODY_LIMIT = 4 * 1024;

export async function acceptInvite(req: Request): Promise<Response> {
  const refused = refuseCrossOrigin(req);
  if (refused) return refused;
  if (!isJsonMediaType(req.headers.get("content-type"))) {
    return errorEnvelope(415, "unsupported_media_type", "Send the body as application/json.");
  }

  const mode = serverAuthMode();
  const token = await upstreamToken(req, mode);
  if (!token) return unauthenticated(req, mode);

  const bytes = await readCapped(req.body, TOKEN_BODY_LIMIT);
  if (!bytes) return errorEnvelope(400, "bad_request", "Send the invite token.");
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    return errorEnvelope(400, "bad_request", "Send the invite token.");
  }
  const inviteToken = (parsed as { token?: unknown } | null)?.token;
  if (typeof inviteToken !== "string" || !inviteToken.trim()) {
    return errorEnvelope(400, "bad_request", "Send the invite token.");
  }

  const res = await callApi(apiUrl("/v1/invites/accept"), {
    method: "POST",
    token,
    json: { token: inviteToken },
    timeoutMs: API_CALL_TIMEOUT_MS,
  });
  if (!answered(res)) return apiFailed(res);
  return passThrough(res, clearCookieOn401(res.status, req, mode));
}
