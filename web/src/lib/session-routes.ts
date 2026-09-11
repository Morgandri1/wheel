// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";
import { errorEnvelope, readCapped } from "@/lib/proxy-rules";
import { serverAuthMode } from "@/lib/runtime-config";
import { refuseCrossOrigin } from "@/lib/same-origin";
import {
  clearedSessionCookie,
  isCookieSafeToken,
  isSecureRequest,
  readSessionToken,
  sessionCookie,
  sessionMaxAge,
} from "@/lib/session-cookie";
import { apiUnreachable, apiUrl, callApi, passThrough, unauthenticated } from "@/lib/upstream";

/**
 * Local-mode sessions, held by this server as an httpOnly cookie.
 *
 * The API issues the JWT; these routes put it in the cookie and hand the browser only `{user}`.
 * Every error the API sends — status, envelope and `retry-after` — reaches the browser unchanged,
 * because the sign-in form's copy (the lockout countdown, the one message for a wrong password
 * and an unknown email) is written against the API's answers, not against ours.
 */

const AUTH_BODY_LIMIT = 16 * 1024;
const NO_STORE = { "cache-control": "no-store" };

export interface SessionUser {
  id: string;
  email: string;
}

/** `GET /api/session` → `{user}`, with `user: null` for no session or a dead one. */
export async function getSession(req: Request): Promise<Response> {
  const token = serverAuthMode() === "local" ? readSessionToken(req) : null;
  if (!token) return Response.json({ user: null }, { headers: NO_STORE });

  const res = await callApi(apiUrl("/v1/auth/me"), { method: "GET", token });
  if (!res) return apiUnreachable();
  if (res.status === 401) {
    return Response.json(
      { user: null },
      { headers: { ...NO_STORE, "set-cookie": clearedSessionCookie(isSecureRequest(req)) } },
    );
  }
  if (!res.ok) return passThrough(res);
  const user = readUser(await res.json().catch(() => null));
  return user ? Response.json({ user }, { headers: NO_STORE }) : unreadableAnswer();
}

/** `POST /api/session/{login|signup|logout|password}`. */
export async function sessionAction(req: Request, action: string): Promise<Response> {
  const refused = refuseCrossOrigin(req);
  if (refused) return refused;
  if (serverAuthMode() !== "local") {
    return errorEnvelope(404, "not_local", "This deployment does not use email and password sign-in.");
  }
  if (action === "login" || action === "signup") return startSession(req, action);
  if (action === "logout") return endSession(req);
  if (action === "password") return changePassword(req);
  return errorEnvelope(404, "not_found", "There is no such session action.");
}

async function startSession(req: Request, action: "login" | "signup"): Promise<Response> {
  const credentials = await readStrings(req, ["email", "password"]);
  if (!credentials) return errorEnvelope(400, "bad_request", "Send an email and a password.");

  const res = await callApi(apiUrl(`/v1/auth/${action}`), { method: "POST", json: credentials });
  if (!res) return apiUnreachable();
  if (!res.ok) return passThrough(res);

  const payload = (await res.json().catch(() => null)) as { token?: unknown; expires_at?: unknown; user?: unknown } | null;
  const user = readUser(payload?.user);
  const token = payload?.token;
  if (!user || typeof token !== "string" || !isCookieSafeToken(token)) return unreadableAnswer();

  const maxAge = sessionMaxAge(payload?.expires_at, token, Date.now());
  if (maxAge === 0) {
    return errorEnvelope(
      502,
      "session_already_expired",
      "The API issued a session that had already expired. Check the API server's clock.",
    );
  }
  const cookie = sessionCookie(token, { secure: isSecureRequest(req), maxAge });
  return Response.json({ user }, { status: res.status, headers: { ...NO_STORE, "set-cookie": cookie } });
}

/**
 * The cookie is cleared whether or not the API answers: a sign-out that leaves the session in the
 * browser because the API blipped is not a sign-out.
 */
async function endSession(req: Request): Promise<Response> {
  const token = readSessionToken(req);
  if (token) await callApi(apiUrl("/v1/auth/logout"), { method: "POST", token });
  return new Response(null, { status: 204, headers: { "set-cookie": clearedSessionCookie(isSecureRequest(req)) } });
}

/** The API revokes every session on a password change, this one included, so the cookie goes too. */
async function changePassword(req: Request): Promise<Response> {
  const token = readSessionToken(req);
  if (!token) return unauthenticated(req, "local");
  const body = await readStrings(req, ["current_password", "new_password"]);
  if (!body) return errorEnvelope(400, "bad_request", "Send the current password and the new one.");

  const res = await callApi(apiUrl("/v1/auth/password"), { method: "POST", token, json: body });
  if (!res) return apiUnreachable();
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

function readUser(value: unknown): SessionUser | null {
  const user = value as { id?: unknown; email?: unknown } | null;
  return typeof user?.id === "string" && typeof user.email === "string" ? { id: user.id, email: user.email } : null;
}

function unreadableAnswer(): Response {
  return errorEnvelope(502, "bad_auth_response", "The API answered the sign-in with something this app can't read.");
}
