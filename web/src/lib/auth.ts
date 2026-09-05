"use client";

/**
 * Token provider shim.
 *
 * NEXT_PUBLIC_AUTH_MODE=mock  → a constant dev token, no Clerk instance needed.
 * NEXT_PUBLIC_AUTH_MODE=dev   → a pre-minted HS256 token from NEXT_PUBLIC_DEV_TOKEN, for running
 *                               the board against the real API before Clerk exists. The secret
 *                               that mints it stays on the server side; only the token is passed.
 * NEXT_PUBLIC_AUTH_MODE=clerk → Clerk's session JWT, fetched per request so it can rotate.
 *
 * Everything else in the app calls getAuthToken(); swapping modes touches only this file.
 */
export type AuthMode = "mock" | "dev" | "clerk";

export const AUTH_MODE: AuthMode =
  (process.env.NEXT_PUBLIC_AUTH_MODE as AuthMode | undefined) ?? "mock";

const MOCK_TOKEN = "mock-session-token";
const DEV_TOKEN = process.env.NEXT_PUBLIC_DEV_TOKEN ?? "";

type TokenGetter = () => Promise<string | null>;

let getter: TokenGetter = async () => (AUTH_MODE === "mock" ? MOCK_TOKEN : null);

function staticToken(): string | null {
  if (AUTH_MODE === "mock") return MOCK_TOKEN;
  if (AUTH_MODE === "dev") return DEV_TOKEN || null;
  return null;
}

/** Called once by the Clerk-aware provider so plain functions can reach the session token. */
export function setTokenGetter(fn: TokenGetter) {
  getter = fn;
}

export async function getAuthToken(): Promise<string> {
  const token = staticToken() ?? (await getter());
  if (!token) {
    throw new ApiError(
      401,
      "unauthenticated",
      AUTH_MODE === "dev"
        ? "Set NEXT_PUBLIC_DEV_TOKEN to a token the API will accept."
        : "Your session expired. Sign in again.",
    );
  }
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
