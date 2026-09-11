// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";
import { publicOrigin } from "@/lib/same-origin";

/**
 * The local-mode session, held where page script cannot read it.
 *
 * The cookie carries the API's own session JWT and nothing else. Who it belongs to is asked of the
 * API when needed (`GET /api/session`), so there is no second copy of the user to drift.
 *
 * HttpOnly, so an XSS cannot exfiltrate it. SameSite=Lax, so it is not sent on cross-site
 * subrequests or POSTs. Path=/. Secure whenever the PUBLIC origin is https — which behind a
 * TLS-terminating proxy is known only from WHEEL_PUBLIC_ORIGIN or a trusted proxy's headers, never
 * from a header anyone could send — and then under the `__Host-` prefix, which a browser accepts
 * only from a secure origin with no Domain, so a sibling subdomain cannot plant or overwrite it.
 */

export const SESSION_COOKIE = "wheel_session";
export const SECURE_SESSION_COOKIE = "__Host-wheel_session";
const COOKIE_SAFE_TOKEN = /^[A-Za-z0-9._~+/=-]{1,4096}$/;

export function isSecureRequest(req: Request): boolean {
  return publicOrigin(req).startsWith("https:");
}

export function sessionCookieName(secure: boolean): string {
  return secure ? SECURE_SESSION_COOKIE : SESSION_COOKIE;
}

export function readCookie(header: string | null, name: string): string | null {
  if (!header) return null;
  for (const part of header.split(";")) {
    const eq = part.indexOf("=");
    if (eq !== -1 && part.slice(0, eq).trim() === name) return part.slice(eq + 1).trim() || null;
  }
  return null;
}

export function readSessionToken(req: Request): string | null {
  return readCookie(req.headers.get("cookie"), sessionCookieName(isSecureRequest(req)));
}

/** The API is trusted, but a value that could break out of a Set-Cookie header is not written into one. */
export function isCookieSafeToken(token: string): boolean {
  return COOKIE_SAFE_TOKEN.test(token);
}

/** Seconds left, from the API's `expires_at` or else the JWT's own `exp`; null when neither says. */
export function sessionMaxAge(expiresAt: unknown, token: string, nowMs: number): number | null {
  const stated = typeof expiresAt === "string" ? Date.parse(expiresAt) : Number.NaN;
  const expiry = Number.isFinite(stated) ? stated : jwtExpiryMs(token);
  if (expiry === null) return null;
  return Math.max(0, Math.floor((expiry - nowMs) / 1000));
}

function jwtExpiryMs(token: string): number | null {
  const payload = token.split(".")[1];
  if (!payload) return null;
  const base64 = payload.replace(/-/g, "+").replace(/_/g, "/");
  try {
    const claims = JSON.parse(atob(base64 + "=".repeat((4 - (base64.length % 4)) % 4))) as { exp?: unknown };
    return typeof claims.exp === "number" && Number.isFinite(claims.exp) ? claims.exp * 1000 : null;
  } catch {
    return null;
  }
}

function attributes(secure: boolean): string {
  return `Path=/; HttpOnly; SameSite=Lax${secure ? "; Secure" : ""}`;
}

export function sessionCookie(token: string, { secure, maxAge }: { secure: boolean; maxAge: number | null }): string {
  return `${sessionCookieName(secure)}=${token}; ${attributes(secure)}${maxAge === null ? "" : `; Max-Age=${maxAge}`}`;
}

export function clearedSessionCookie(secure: boolean): string {
  return `${sessionCookieName(secure)}=; ${attributes(secure)}; Max-Age=0`;
}

/**
 * Where middleware sends a visitor to /app who holds no session cookie. A routing courtesy only:
 * a cookie that is present but dead still reaches the page, and the API refuses it there.
 */
export function signInRedirect(pathname: string, hasSession: boolean): string | null {
  if (hasSession) return null;
  if (pathname === "/app") return "/sign-in";
  if (!pathname.startsWith("/app/")) return null;
  return `/sign-in?next=${encodeURIComponent(pathname)}`;
}
