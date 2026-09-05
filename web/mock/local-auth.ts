import { randomUUID } from "node:crypto";
import { EngineRefusal } from "./state";

/**
 * Local email/password auth, mock edition (§ API AUTH_MODE=local).
 *
 * This exists so the sign-in, sign-up, rate-limit and expired-session paths in the UI are
 * exercised in development rather than discovered in production. It is NOT a model of the API's
 * security: passwords are compared in plaintext here and the token is a random id rather than a
 * signed JWT, because the web treats the token as opaque and would not notice the difference.
 * The parts that ARE modelled faithfully are the parts the UI has to handle — status codes, error
 * codes, Retry-After, and the fact that a wrong email and a wrong password answer identically.
 */

interface MockUser {
  id: string;
  email: string;
  password: string;
}

const MIN_PASSWORD = 10;
const MAX_ATTEMPTS = 5;
const LOCKOUT_SECONDS = 30;

const users = new Map<string, MockUser>();
const tokens = new Map<string, string>();
const attempts = new Map<string, { count: number; until: number }>();

const normalise = (email: unknown) => String(email ?? "").trim().toLowerCase();

/** A signed-in account to develop against, so the first run does not start with a sign-up form. */
export function seedUser() {
  const email = "dev@wheel.dev";
  if (!users.has(email)) {
    users.set(email, { id: randomUUID(), email, password: "wheel-dev-password" });
  }
}

/** Local tokens are checked; anything else (mock/dev modes) is waved through by the caller. */
export function isLocalToken(token: string) {
  return token.startsWith("local.");
}

export function userForToken(token: string): string | null {
  return tokens.get(token) ?? null;
}

function issue(user: MockUser) {
  const token = `local.${randomUUID()}`;
  tokens.set(token, user.id);
  return { token, user: { id: user.id, email: user.email } };
}

function assertCredentialShape(email: string, password: unknown) {
  if (!/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(email)) {
    throw new EngineRefusal(400, "That doesn't look like an email address.", "invalid_email");
  }
  if (typeof password !== "string" || password.length < MIN_PASSWORD) {
    throw new EngineRefusal(400, `Use at least ${MIN_PASSWORD} characters.`, "weak_password");
  }
}

export function signup(body: { email?: unknown; password?: unknown }) {
  const email = normalise(body.email);
  assertCredentialShape(email, body.password);
  if (users.has(email)) {
    throw new EngineRefusal(409, "There's already an account with that email.", "email_taken");
  }
  const user: MockUser = { id: randomUUID(), email, password: String(body.password) };
  users.set(email, user);
  return issue(user);
}

export function login(body: { email?: unknown; password?: unknown }) {
  const email = normalise(body.email);
  const record = attempts.get(email);
  const now = Date.now();

  if (record && record.until > now) {
    const seconds = Math.ceil((record.until - now) / 1000);
    throw new EngineRefusal(429, `Sign-in is paused for this account. Try again in ${seconds} seconds.`, "rate_limited", {
      "retry-after": String(seconds),
    });
  }

  const user = users.get(email);
  // Wrong email and wrong password are the same answer. Anything else is an account enumerator.
  if (!user || user.password !== String(body.password ?? "")) {
    // `until: 0` means "no lockout", so only an EXPIRED lockout resets the count. Treating the
    // two as one is why this counter never reached the limit the first time around.
    const expired = record !== undefined && record.until > 0 && record.until <= now;
    const count = (expired ? 0 : record?.count ?? 0) + 1;
    attempts.set(email, {
      count,
      until: count >= MAX_ATTEMPTS ? now + LOCKOUT_SECONDS * 1000 : 0,
    });
    throw new EngineRefusal(401, "That email and password don't match an account.", "invalid_credentials");
  }

  attempts.delete(email);
  return issue(user);
}

export function logout(token: string) {
  tokens.delete(token);
}

export function me(token: string) {
  const id = tokens.get(token);
  const user = id ? [...users.values()].find((u) => u.id === id) : undefined;
  if (!user) throw new EngineRefusal(401, "no session", "unauthenticated");
  return { id: user.id, email: user.email };
}
