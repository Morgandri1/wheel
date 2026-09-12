// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import type { BrowserContext } from "@playwright/test";

/**
 * Put a signed-in session in place without driving the sign-in form.
 *
 * Web's suggestion, and the reason for it is the good part: with every downstream spec
 * typing into the form, a broken form fails all of them at once, and a downstream green
 * tells you nothing about its own subject. Seeding keeps local-auth.spec.ts as the single
 * place a sign-in failure can come from.
 *
 * THE COOKIE MODEL (web/server-side-api). The session is an httpOnly cookie the web server
 * sets from the API's JWT; page script can neither read nor write it, and there is no
 * localStorage copy any more. So a seed is a REAL token — from `apiLogin()` against the API
 * the web server talks to — placed in a cookie shaped exactly like the one the server sets.
 * A made-up token would be refused by the first `/api/session` check and prove nothing.
 *
 * THE HAZARD THIS BRINGS, and why SEEDED_SHAPE exists. A seeded session is a fake of the
 * app's own state. If the real cookie changes — a renamed cookie, a dropped HttpOnly, a
 * different SameSite — every seeded spec goes on passing against a shape the app no longer
 * produces. `E2E-local-session-shape` in local-auth.spec.ts signs in FOR REAL and asserts the
 * cookie that lands matches this shape (name and flags, never the value), and that the old
 * localStorage mirror has not come back.
 */
export const SESSION_COOKIE = "wheel_session";

/** What the web server sets over plain http, as Playwright reports a cookie. */
export const SEEDED_SHAPE = {
  name: SESSION_COOKIE,
  path: "/",
  httpOnly: true,
  secure: false,
  sameSite: "Lax" as const,
};

/** The retired localStorage key. Asserted absent, so the mirror cannot quietly return. */
export const RETIRED_STORAGE_KEY = "wheel.session";

/** A real session token, minted by the API itself — the only kind a seeded cookie can use. */
export async function apiLogin(api: string, email: string, password: string): Promise<string> {
  const res = await fetch(`${api}/v1/auth/login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ email, password }),
  });
  if (!res.ok) throw new Error(`POST /v1/auth/login -> ${res.status} ${await res.text()}`);
  return ((await res.json()) as { token: string }).token;
}

/** Cookies are per-context and survive navigation, so the app sees the session on first paint. */
export async function seedSession(context: BrowserContext, baseURL: string, token: string) {
  await context.addCookies([
    { ...SEEDED_SHAPE, value: token, domain: new URL(baseURL).hostname, expires: -1 },
  ]);
}
