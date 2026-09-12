"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * The browser's view of auth: which mode the server is running, and what to do when the API says
 * the session is over.
 *
 * The browser holds no token in any mode. This app's server attaches the credential to each API
 * call (`src/lib/upstream.ts`) — the local session cookie, Clerk's server-side token, or the
 * dev/mock token from server-only env — which keeps every token out of reach of page script.
 *
 * The mode is decided by the server at run time (WHEEL_AUTH_MODE) and recorded here by
 * <RuntimeConfig> before anything below it renders, so one prebuilt bundle follows its environment.
 */
export type AuthMode = "mock" | "dev" | "local" | "clerk";

let mode: AuthMode = "mock";

export function setAuthMode(next: AuthMode) {
  mode = next;
}

export function authMode(): AuthMode {
  return mode;
}

let onUnauthorized: () => void = () => {};

/**
 * Registered by the session owner. A 401 from ANY route — not only the auth ones — means the
 * session is no longer worth anything, so the app stops acting as if it had one.
 */
export function setUnauthorizedHandler(fn: () => void) {
  onUnauthorized = fn;
}

export function notifyUnauthorized() {
  onUnauthorized();
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
