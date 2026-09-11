// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";
import { errorEnvelope, isJsonMediaType, readCapped } from "@/lib/proxy-rules";
import { serverAuthMode } from "@/lib/runtime-config";
import { refuseCrossOrigin } from "@/lib/same-origin";
import {
  clearedSessionCookie,
  isCookieSafeToken,
  isLiveSessionToken,
  isSecureRequest,
  liveSessionToken,
  readSessionToken,
  sessionCookie,
  sessionMaxAge,
} from "@/lib/session-cookie";
import { readUser } from "@/lib/session-user";
import { API_CALL_TIMEOUT_MS, answered, apiFailed, apiUrl, callApi, passThrough, unauthenticated } from "@/lib/upstream";

/**
 * Local-mode sessions, held by this server as an httpOnly cookie (web/DEPLOY.md, "The trust
 * model"). Every error the API sends — status, envelope and `retry-after` — reaches the browser
 * unchanged, because the sign-in form's copy is written against the API's answers, not ours.
 */

const AUTH_BODY_LIMIT = 16 * 1024;
const NO_STORE = { "cache-control": "no-store" };
const timeoutMs = API_CALL_TIMEOUT_MS;

/** `GET /api/session` → `{user}`, with `user: null` only when there is no session or the API says it is dead. */
export async function getSession(req: Request): Promise<Response> {
  const refused = refuseCrossOrigin(req);
  if (refused) return refused;
  if (serverAuthMode() !== "local") return noUser();
  const cookie = readSessionToken(req);
  if (cookie === null) return noUser();
  const cleared = clearedSessionCookie(isSecureRequest(req));
  // Not a live JWT: not worth a round trip, and not worth keeping.
  if (!isLiveSessionToken(cookie, Date.now())) return noUser(cleared);

  const res = await callApi(apiUrl("/v1/auth/me"), { method: "GET", token: cookie, timeoutMs });
  if (!answered(res)) return apiFailed(res);
  if (res.status === 401) return noUser(cleared);
  if (!res.ok) return passThrough(res);
  const user = readUser(await res.json().catch(() => null));
  return user ? Response.json({ user }, { headers: NO_STORE }) : unreadableAnswer();
}

function noUser(clearCookie?: string): Response {
  return Response.json({ user: null }, { headers: clearCookie ? { ...NO_STORE, "set-cookie": clearCookie } : NO_STORE });
}

/** `POST /api/session/{login|signup|logout|password}`. */
export async function sessionAction(req: Request, action: string): Promise<Response> {
  const refused = refuseCrossOrigin(req);
  if (refused) return refused;
  if (serverAuthMode() !== "local") {
    return errorEnvelope(404, "not_local", "This deployment does not use email and password sign-in.");
  }
  if (action === "logout") return endSession(req);
  if (action !== "login" && action !== "signup" && action !== "password") {
    return errorEnvelope(404, "not_found", "There is no such session action.");
  }
  if (!isJsonMediaType(req.headers.get("content-type"))) {
    return errorEnvelope(415, "unsupported_media_type", "Send the body as application/json.");
  }
  return action === "password" ? changePassword(req) : startSession(req, action);
}

async function startSession(req: Request, action: "login" | "signup"): Promise<Response> {
  const credentials = await readStrings(req, ["email", "password"]);
  if (!credentials) return errorEnvelope(400, "bad_request", "Send an email and a password.");

  const res = await callApi(apiUrl(`/v1/auth/${action}`), { method: "POST", json: credentials, timeoutMs });
  if (!answered(res)) return apiFailed(res);
  if (!res.ok) return passThrough(res);

  const payload = (await res.json().catch(() => null)) as { token?: unknown; expires_at?: unknown; user?: unknown } | null;
  const user = readUser(payload?.user);
  const token = payload?.token;
  if (!user || typeof token !== "string" || !isCookieSafeToken(token)) return unreadableAnswer();

  const now = Date.now();
  const maxAge = sessionMaxAge(payload?.expires_at, token, now);
  if (maxAge === 0 || !isLiveSessionToken(token, now)) {
    return errorEnvelope(
      502,
      "session_unusable",
      "The API issued a session this app can't use: not a JWT, or already expired. Check the API server's clock.",
    );
  }
  const cookie = sessionCookie(token, { secure: isSecureRequest(req), maxAge });
  return Response.json({ user }, { status: res.status, headers: { ...NO_STORE, "set-cookie": cookie } });
}

/** The cookie is cleared whether or not the API answers: a sign-out that depends on the API is not one. */
async function endSession(req: Request): Promise<Response> {
  const token = liveSessionToken(req);
  if (token) await callApi(apiUrl("/v1/auth/logout"), { method: "POST", token, timeoutMs });
  return new Response(null, { status: 204, headers: { "set-cookie": clearedSessionCookie(isSecureRequest(req)) } });
}

/** The API revokes every session on a password change, this one included, so the cookie goes too. */
async function changePassword(req: Request): Promise<Response> {
  const token = liveSessionToken(req);
  if (!token) return unauthenticated(req, "local");
  const body = await readStrings(req, ["current_password", "new_password"]);
  if (!body) return errorEnvelope(400, "bad_request", "Send the current password and the new one.");

  const res = await callApi(apiUrl("/v1/auth/password"), { method: "POST", token, json: body, timeoutMs });
  if (!answered(res)) return apiFailed(res);
  const cleared = clearedSessionCookie(isSecureRequest(req));
  if (res.ok) return new Response(null, { status: 204, headers: { "set-cookie": cleared } });
  return passThrough(res, res.status === 401 ? [["set-cookie", cleared]] : []);
}

/** Exactly the named string fields and nothing else, so the API never sees a field we did not mean to send. */
async function readStrings<K extends string>(req: Request, keys: K[]): Promise<Record<K, string> | null> {
  const bytes = await readCapped(req.body, AUTH_BODY_LIMIT);
  if (!bytes) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object") return null;
  const out = {} as Record<K, string>;
  for (const key of keys) {
    const value = (parsed as Record<string, unknown>)[key];
    if (typeof value !== "string") return null;
    out[key] = value;
  }
  return out;
}

function unreadableAnswer(): Response {
  return errorEnvelope(502, "bad_auth_response", "The API answered the sign-in with something this app can't read.");
}
