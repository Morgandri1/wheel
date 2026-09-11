"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { useSyncExternalStore } from "react";
import { ApiError, setUnauthorizedHandler } from "@/lib/auth";

/**
 * Local email/password sessions (WHEEL_AUTH_MODE=local), as the browser sees them.
 *
 * The API issues an HS256 session JWT; this app's server keeps it in an httpOnly cookie
 * (`src/lib/session-routes.ts`) and attaches it to every API call. The browser holds only WHO is
 * signed in — never the token — so an XSS on this origin cannot carry a session away. Script can
 * still act as the user while the page is open; the CSP is what bounds that.
 *
 * QA's `E2E-local-session-shape` signs in for real and compares the cookie that lands against
 * `SEEDED_SHAPE` in `qa/e2e/session.ts`. Changing the cookie's name or flags turns it red ON
 * PURPOSE: update the fake, do not route around it.
 *
 * SIGNED IN IS NOT THE SAME AS NOT-YET-KNOWN. The snapshot starts `loading` and becomes `anon` only
 * after the server has answered, because a gate that cannot tell those apart bounces every
 * returning user to the sign-in page for one frame.
 */

export interface SessionUser {
  id: string;
  email: string;
}

export type SessionState =
  | { status: "loading"; user: null }
  | { status: "anon"; user: null }
  | { status: "authed"; user: SessionUser };

const SESSION_ROUTE = "/api/session";
/** The API is the authority on this; the client check exists so the round trip is not the teacher. */
export const MIN_PASSWORD_LENGTH = 10;

const LOADING: SessionState = { status: "loading", user: null };
const ANON: SessionState = { status: "anon", user: null };

let state: SessionState = LOADING;
let hydrating: Promise<void> | null = null;
const listeners = new Set<() => void>();

function publish(next: SessionState) {
  state = next;
  for (const listener of listeners) listener();
}

function readUser(value: unknown): SessionUser | null {
  const user = value as { id?: unknown; email?: unknown } | null | undefined;
  return typeof user?.id === "string" && typeof user.email === "string" ? { id: user.id, email: user.email } : null;
}

/**
 * Asks the server who the cookie belongs to. Called by the local-mode gate; safe to call again.
 * A sign-in that lands while this is in flight wins: only a still-`loading` state is overwritten.
 */
export function hydrateSession(): Promise<void> {
  if (state !== LOADING) return Promise.resolve();
  hydrating ??= (async () => {
    let user: SessionUser | null = null;
    try {
      const res = await fetch(SESSION_ROUTE, { cache: "no-store", credentials: "same-origin" });
      if (res.ok) user = readUser(((await res.json()) as { user?: unknown })?.user);
    } catch {
      // Unreachable reads as signed out. The sign-in form then says "can't reach" when it is used,
      // which names the real problem; a gate stuck on "checking" would not.
    }
    if (state === LOADING) publish(user ? { status: "authed", user } : ANON);
  })();
  return hydrating;
}

export function clearSession() {
  if (state === ANON) return;
  publish(ANON);
}

export function subscribeSession(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export const sessionSnapshot = (): SessionState => state;
const serverSnapshot = () => LOADING;

export function useSession(): SessionState {
  return useSyncExternalStore(subscribeSession, sessionSnapshot, serverSnapshot);
}

// ── talking to the session routes ───────────────────────────────────────────

async function sessionRequest(action: string, body: unknown = {}): Promise<unknown> {
  let res: Response;
  try {
    res = await fetch(`${SESSION_ROUTE}/${action}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      credentials: "same-origin",
      body: JSON.stringify(body),
    });
  } catch {
    throw new ApiError(0, "offline", "Can't reach this app's server. Check your connection.");
  }

  if (!res.ok) throw await authError(res);
  if (res.status === 204) return undefined;
  return await res.json().catch(() => undefined);
}

/** The server passes the API's status, envelope and `retry-after` through untouched, so this reads the API's words. */
async function authError(res: Response): Promise<ApiError> {
  let code = `http_${res.status}`;
  let message = "";
  try {
    const body = (await res.json()) as { error?: { code?: string; message?: string } };
    if (body?.error?.code) code = body.error.code;
    if (body?.error?.message) message = body.error.message;
  } catch {
    /* fall through to our own copy */
  }
  if (res.status === 429) {
    // API: the login limit is keyed per EMAIL, not per browser — so the person reading this may
    // be throttled by someone else attacking their account. The copy says what to do and does
    // not accuse them of anything.
    const after = Number(res.headers.get("retry-after"));
    message =
      message ||
      (Number.isFinite(after) && after > 0
        ? `Sign-in is paused for this account. Try again in ${Math.ceil(after)} seconds.`
        : "Sign-in is paused for this account. Try again shortly.");
  }
  if (!message) message = fallbackMessage(res.status);
  return new ApiError(res.status, code, message);
}

function fallbackMessage(status: number): string {
  if (status === 401) return "That email and password don't match an account.";
  if (status === 409) return "There's already an account with that email.";
  if (status === 400) return "Check the email and password and try again.";
  if (status >= 500) return "The API failed. Try again in a moment.";
  return "That didn't work.";
}

/** Client-side gate on the API's own rule, so the length is taught before a round trip, not after. */
export function passwordProblem(password: string): string | null {
  if (password.length === 0) return "Enter your password.";
  if (password.length < MIN_PASSWORD_LENGTH) {
    return `Use at least ${MIN_PASSWORD_LENGTH} characters — that's ${MIN_PASSWORD_LENGTH - password.length} more.`;
  }
  return null;
}

export function emailProblem(email: string): string | null {
  if (!email.trim()) return "Enter your email.";
  if (!/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(email.trim())) return "That doesn't look like an email address.";
  return null;
}

async function startSession(action: "login" | "signup", email: string, password: string): Promise<SessionUser> {
  const payload = await sessionRequest(action, { email: email.trim(), password });
  const user = readUser((payload as { user?: unknown } | undefined)?.user);
  if (!user) throw new ApiError(502, "bad_auth_response", "The server answered the sign-in with something this app can't read.");
  publish({ status: "authed", user });
  return user;
}

export function signUp(email: string, password: string): Promise<SessionUser> {
  return startSession("signup", email, password);
}

export function signIn(email: string, password: string): Promise<SessionUser> {
  return startSession("login", email, password);
}

/**
 * Change the password, then end the local session — because the API has already ended every one.
 *
 * `POST /v1/auth/password` revokes EVERY session including the caller's own (docs/API.md), which is
 * the point: a password changed because it leaked must not leave the leaked sessions alive. The
 * server clears the cookie on success, and the UI follows.
 *
 * The current password is required even though the caller is authenticated, so a hijacked page
 * cannot be turned into a permanent takeover.
 */
export async function changePassword(currentPassword: string, newPassword: string): Promise<void> {
  if (state.status !== "authed") throw new Error("You are not signed in.");
  await sessionRequest("password", { current_password: currentPassword, new_password: newPassword });
  publish(ANON);
}

/**
 * The server clears the cookie whether or not the API answers, and the UI is signed out whether
 * or not the server does. The cookie is cleared FIRST, so a navigation that follows sign-out can
 * never race an in-flight request still carrying it.
 */
export async function signOut(): Promise<void> {
  try {
    await sessionRequest("logout");
  } catch {
    /* the UI still signs out below; that is the part the user asked for */
  } finally {
    publish(ANON);
  }
}

// Any 401 from anywhere means this session is over — including one from a route that has nothing
// to do with auth. In the other modes nothing reads this state, so registering it is harmless.
setUnauthorizedHandler(clearSession);
