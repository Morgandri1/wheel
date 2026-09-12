// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/** Who is signed in. The browser holds this and nothing more; the token stays in the cookie. */
export interface SessionUser {
  id: string;
  email: string;
}

/** The one reader of a user off the wire, shared by the session routes and the browser's session store. */
export function readUser(value: unknown): SessionUser | null {
  const user = value as { id?: unknown; email?: unknown } | null | undefined;
  return typeof user?.id === "string" && typeof user.email === "string" ? { id: user.id, email: user.email } : null;
}
