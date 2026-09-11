"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { useSyncExternalStore } from "react";
import { ApiError, setUnauthorizedHandler } from "@/lib/auth";
import { readUser, type SessionUser } from "@/lib/session-user";

export type { SessionUser };

/**
 * Local email/password sessions (WHEEL_AUTH_MODE=local), as the browser sees them. The cookie holds
 * the token; this module holds only WHO is signed in (web/DEPLOY.md, "The trust model").
 *
 * QA's `E2E-local-session-shape` signs in for real and compares the cookie that lands against
 * `SEEDED_SHAPE` in `qa/e2e/session.ts`. Changing the cookie's name or flags turns it red ON
 * PURPOSE: update the fake, do not route around it.
 *
 * FIVE STATES, and the differences are the point. `loading` has not asked yet; `anon` means the
 * server said `{user: null}`; `unreachable` means a 5xx, a timeout or a network failure — retried,
 * because that kind of failure passes; `error` means the server answered but refused the request
 * (most often 403 `public_origin_required` from a deployment missing `WHEEL_PUBLIC_ORIGIN`) — a
 * config problem retrying will not fix, so its message is shown instead of "can't reach the
 * server"; `authed` names the user.
 */

export type SessionState =
  | { status: "loading"; user: null }
  | { status: "anon"; user: null }
  | { status: "unreachable"; user: null }
  | { status: "error"; user: null; message: string }
  | { status: "authed"; user: SessionUser };

const SESSION_ROUTE = "/api/session";
/** The API is the authority on this; the client check exists so the round trip is not the teacher. */
export const MIN_PASSWORD_LENGTH = 10;
const RETRY_MS = [1_000, 2_000, 5_000, 10_000, 30_000] as const;

const LOADING: SessionState = { status: "loading", user: null };
const ANON: SessionState = { status: "anon", user: null };
const UNREACHABLE: SessionState = { status: "unreachable", user: null };

let state: SessionState = LOADING;
let asking: Promise<void> | null = null;
let retries = 0;
let retryTimer: ReturnType<typeof setTimeout> | null = null;
const listeners = new Set<() => void>();

function publish(next: SessionState) {
  state = next;
  for (const listener of listeners) listener();
}

const unsettled = () => state.status === "loading" || state.status === "unreachable" || state.status === "error";

/**
 * The server's verdict, or null when this attempt learned nothing and should be retried: a 5xx, a
 * timeout, or the fetch itself failing. Only an explicit `{user: null}` means signed out, and only
 * a 4xx becomes `error` — that is the server refusing the request outright, not a blip, so it is
 * shown rather than swallowed into "can't reach the server" and retried forever.
 */
async function askServer(): Promise<SessionState | null> {
  let res: Response;
  try {
    res = await fetch(SESSION_ROUTE, { cache: "no-store", credentials: "same-origin" });
  } catch {
    return null;
  }
  if (res.status >= 500) return null;
  if (!res.ok) return { status: "error", user: null, message: await readErrorMessage(res) };
  const body = (await res.json().catch(() => null)) as { user?: unknown } | null;
  if (body?.user === null) return ANON;
  const user = readUser(body?.user);
  return user ? { status: "authed", user } : null;
}

async function readErrorMessage(res: Response): Promise<string> {
  try {
    const body = (await res.json()) as { error?: { message?: unknown } };
    if (typeof body?.error?.message === "string" && body.error.message) return body.error.message;
  } catch {
    /* fall through to the generic line below */
  }
  return `The server refused this request (HTTP ${res.status}).`;
}

/**
 * Asks the server who the cookie belongs to. Called by the local-mode gate; safe to call again.
 * A sign-in that lands while this is in flight wins.
 */
export function hydrateSession(): Promise<void> {
  if (!unsettled()) return Promise.resolve();
  asking ??= askServer().then((verdict) => {
    asking = null;
    if (!unsettled()) return;
    if (verdict) {
      retries = 0;
      publish(verdict);
      return;
    }
    publish(UNREACHABLE);
    scheduleRetry();
  });
  return asking;
}

function scheduleRetry() {
  if (retryTimer) return;
  const wait = RETRY_MS[Math.min(retries, RETRY_MS.length - 1)]!;
  retries += 1;
  retryTimer = setTimeout(() => {
    retryTimer = null;
    void hydrateSession();
  }, wait);
}

/** The gate's "try now": ask immediately instead of waiting out the backoff. */
export function retrySession(): Promise<void> {
  if (retryTimer) clearTimeout(retryTimer);
  retryTimer = null;
  return hydrateSession();
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
