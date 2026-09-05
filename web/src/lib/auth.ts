"use client";

/**
 * Token provider shim.
 *
 * NEXT_PUBLIC_AUTH_MODE=mock  → a constant dev token, no Clerk instance needed.
 * NEXT_PUBLIC_AUTH_MODE=clerk → Clerk's session JWT, fetched per request so it can rotate.
 *
 * Everything else in the app calls getAuthToken(); swapping modes touches only this file.
 */
export type AuthMode = "mock" | "clerk";

export const AUTH_MODE: AuthMode =
  (process.env.NEXT_PUBLIC_AUTH_MODE as AuthMode | undefined) ?? "mock";

const MOCK_TOKEN = "mock-session-token";

type TokenGetter = () => Promise<string | null>;

let getter: TokenGetter = async () => (AUTH_MODE === "mock" ? MOCK_TOKEN : null);

/** Called once by the Clerk-aware provider so plain functions can reach the session token. */
export function setTokenGetter(fn: TokenGetter) {
  getter = fn;
}

export async function getAuthToken(): Promise<string> {
  const token = AUTH_MODE === "mock" ? MOCK_TOKEN : await getter();
  if (!token) throw new ApiError(401, "unauthenticated", "Your session expired. Sign in again.");
  return token;
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}
