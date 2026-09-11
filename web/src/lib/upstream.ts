// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";
import type { AuthMode } from "@/lib/auth";
import { apiTarget, errorEnvelope, returnedResponseHeaders } from "@/lib/proxy-rules";
import { devToken, serverApiBaseUrl, serverAuthMode } from "@/lib/runtime-config";
import { clearedSessionCookie, isSecureRequest, readSessionToken } from "@/lib/session-cookie";

/**
 * This server's side of every call to the API: which credential it presents on the caller's
 * behalf, and how the API's answer is handed back. The credential never travels to the browser.
 */

export const MOCK_TOKEN = "mock-session-token";
const BODYLESS_STATUS = new Set([101, 204, 205, 304]);

export async function upstreamToken(req: Request, mode: AuthMode = serverAuthMode()): Promise<string | null> {
  if (mode === "local") return readSessionToken(req);
  if (mode === "clerk") return clerkToken();
  if (mode === "dev") return devToken();
  return devToken() ?? MOCK_TOKEN;
}

async function clerkToken(): Promise<string | null> {
  const { auth } = await import("@clerk/nextjs/server");
  const { getToken } = await auth();
  return getToken();
}

/** A path on the fixed API origin. Only for paths this server builds itself. */
export function apiUrl(path: string): string {
  const target = apiTarget(serverApiBaseUrl(), path);
  if (!target) throw new Error(`refusing to build an API url for ${JSON.stringify(path)}`);
  return target;
}

interface ApiCall {
  method: string;
  token?: string;
  projectId?: string;
  json?: unknown;
  body?: BodyInit;
  headers?: Headers;
  signal?: AbortSignal;
}

/** The API's response, or null when it could not be reached at all. Redirects are never followed. */
export async function callApi(target: string, call: ApiCall): Promise<Response | null> {
  const headers = new Headers(call.headers);
  if (call.token) headers.set("x-auth-token", call.token);
  if (call.projectId) headers.set("x-project-id", call.projectId);
  let body = call.body;
  if (call.json !== undefined) {
    headers.set("content-type", "application/json");
    body = JSON.stringify(call.json);
  }
  try {
    return await fetch(target, {
      method: call.method,
      headers,
      body,
      redirect: "manual",
      cache: "no-store",
      signal: call.signal,
    });
  } catch {
    return null;
  }
}

export function apiUnreachable(): Response {
  return errorEnvelope(502, "api_unreachable", "Can't reach the API. Check that it's running.");
}

/** The API's answer, status and error envelope untouched, with only the headers the browser needs. */
export function passThrough(res: Response, extraHeaders: [string, string][] = []): Response {
  const headers = returnedResponseHeaders(res.headers);
  for (const [name, value] of extraHeaders) headers.append(name, value);
  return new Response(BODYLESS_STATUS.has(res.status) ? null : res.body, {
    status: res.status,
    statusText: res.statusText,
    headers,
  });
}

export function unauthenticated(req: Request, mode: AuthMode): Response {
  const message =
    mode === "dev"
      ? "Set WHEEL_DEV_TOKEN on the web server to a token the API accepts."
      : "You're signed out. Sign in again.";
  return errorEnvelope(401, "unauthenticated", message, clearCookieOn401(401, req, mode));
}

/** A dead session is removed where it is noticed, not left for the next request to trip on. */
export function clearCookieOn401(status: number, req: Request, mode: AuthMode): [string, string][] {
  return status === 401 && mode === "local" ? [["set-cookie", clearedSessionCookie(isSecureRequest(req))]] : [];
}
