"use client";

import { useSyncExternalStore } from "react";
import { AUTH_MODE, ApiError, setTokenGetter, setUnauthorizedHandler } from "@/lib/auth";
import { API_URL } from "@/lib/api";

/**
 * Local email/password sessions (NEXT_PUBLIC_AUTH_MODE=local).
 *
 * The API owns the auth boundary: it issues an HS256 session JWT from /v1/auth/login|signup and
 * every project-scoped call carries it as `x-auth-token`, exactly as a Clerk or Privy token would.
 * This file is the whole difference between providers — it holds the token, hands it to the API
 * client through the same shim Clerk uses, and throws it away the moment the API says it is dead.
 *
 * WHERE THE TOKEN LIVES, and the tradeoff, stated plainly because it is a real one:
 * in memory, mirrored to localStorage so a reload does not sign you out. localStorage is readable
 * by ANY script running on this origin, so an XSS anywhere in the app is a stolen session — a
 * token in memory alone would die with the tab, and an httpOnly SameSite=None cookie would be
 * unreadable by script entirely. We are not using the cookie today because the API and the web
 * are on different origins, which makes a cookie a CSRF surface the API must then defend, and
 * because it is the API's call to make, not the web's. The exposure is bounded by what the token
 * can do (one user's own projects) and by its expiry. Documented for ADVERSARY in web/DEPLOY.md;
 * if the API ships a cookie, this file switches to credentials:"include" and drops the mirror.
 *
 * SIGNED IN IS NOT THE SAME AS NOT-YET-KNOWN. The snapshot starts `loading` and only becomes
 * `anon` after we have actually looked in storage, because a gate that cannot tell those apart
 * bounces every returning user to the sign-in page for one frame.
 */

export interface SessionUser {
  id: string;
  email: string;
}

interface StoredSession {
  token: string;
  user: SessionUser;
}

export type SessionState =
  | { status: "loading"; user: null }
  | { status: "anon"; user: null }
  | { status: "authed"; user: SessionUser };

const STORAGE_KEY = "wheel.session";
/** The API is the authority on this; the client check exists so the round trip is not the teacher. */
export const MIN_PASSWORD_LENGTH = 10;

const LOADING: SessionState = { status: "loading", user: null };
const ANON: SessionState = { status: "anon", user: null };

let session: StoredSession | null = null;
let state: SessionState = LOADING;
const listeners = new Set<() => void>();

function publish(next: SessionState) {
  state = next;
  for (const listener of listeners) listener();
}

function persist(next: StoredSession | null) {
  session = next;
  try {
    if (next) window.localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
    else window.localStorage.removeItem(STORAGE_KEY);
  } catch {
    // Private mode, or storage full. The session still works for this tab; it just will not survive
    // a reload, which is a worse experience and not a broken one.
  }
  publish(next ? { status: "authed", user: next.user } : ANON);
}

function readStored(): StoredSession | null {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<StoredSession>;
    if (typeof parsed?.token !== "string" || !parsed.token) return null;
    if (typeof parsed.user?.id !== "string" || typeof parsed.user?.email !== "string") return null;
    return { token: parsed.token, user: { id: parsed.user.id, email: parsed.user.email } };
  } catch {
    return null;
  }
}

/** Called once, client-side, by the local-mode gate. Safe to call again. */
export function hydrateSession() {
  if (state !== LOADING) return;
  const stored = readStored();
  session = stored;
  publish(stored ? { status: "authed", user: stored.user } : ANON);
}

export function clearSession() {
  if (session === null && state !== LOADING) return;
  persist(null);
}

export function sessionToken(): string | null {
  return session?.token ?? null;
}

export function subscribeSession(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

const snapshot = () => state;
const serverSnapshot = () => LOADING;

export function useSession(): SessionState {
  return useSyncExternalStore(subscribeSession, snapshot, serverSnapshot);
}

// ── talking to the API ──────────────────────────────────────────────────────

/**
 * Every response shape the auth routes can return is read here and nowhere else, so correcting a
 * guess about the API's wire format is a change to one function rather than a hunt.
 */
function readSession(payload: unknown): StoredSession {
  const body = payload as { token?: unknown; user?: { id?: unknown; email?: unknown } };
  if (typeof body?.token !== "string" || typeof body.user?.id !== "string" || typeof body.user?.email !== "string") {
    throw new ApiError(502, "bad_auth_response", "The API answered the sign-in with something this app can't read.");
  }
  return { token: body.token, user: { id: body.user.id, email: body.user.email } };
}

async function authRequest(path: string, body: unknown, token?: string): Promise<unknown> {
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (token) headers["x-auth-token"] = token;

  let res: Response;
  try {
    res = await fetch(`${API_URL}${path}`, { method: "POST", headers, body: JSON.stringify(body) });
  } catch {
    throw new ApiError(0, "offline", "Can't reach the API. Check that it's running.");
  }

  if (!res.ok) throw await authError(res);
  if (res.status === 204) return undefined;
  return await res.json().catch(() => undefined);
}

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
    const after = Number(res.headers.get("retry-after"));
    message =
      message ||
      (Number.isFinite(after) && after > 0
        ? `Too many attempts. Try again in ${Math.ceil(after)} seconds.`
        : "Too many attempts. Wait a moment and try again.");
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

export async function signUp(email: string, password: string): Promise<SessionUser> {
  const next = readSession(await authRequest("/v1/auth/signup", { email: email.trim(), password }));
  persist(next);
  return next.user;
}

export async function signIn(email: string, password: string): Promise<SessionUser> {
  const next = readSession(await authRequest("/v1/auth/login", { email: email.trim(), password }));
  persist(next);
  return next.user;
}

/**
 * Tell the API first, then forget locally — but forget locally even if the API call fails, because
 * a sign-out that leaves the token in the browser because the network blipped is not a sign-out.
 */
export async function signOut(): Promise<void> {
  const token = session?.token;
  persist(null);
  if (!token) return;
  try {
    await authRequest("/v1/auth/logout", {}, token);
  } catch {
    /* the local session is already gone; that is the part the user asked for */
  }
}

// The API client reaches the token through the same shim Clerk uses, and any 401 from anywhere
// means this session is over — including one from a route that has nothing to do with auth.
//
// Guarded, because this module is imported by the /app gate in every mode: registering
// unconditionally would let it overwrite the getter Clerk installs from its own effect, and the
// board would go quietly unauthenticated in the one mode we cannot test locally.
if (AUTH_MODE === "local") {
  setTokenGetter(async () => sessionToken());
  setUnauthorizedHandler(clearSession);
}
