// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";
import type { AuthMode } from "@/lib/auth";

/**
 * Where the API lives and which auth mode is in force, resolved on the SERVER at run time.
 *
 * The browser never learns the API's address: every call goes through this app's own route
 * handlers, which is what lets the API bind to loopback or a private network. `server-only`
 * turns a client import of this module into a build error instead of a leak.
 *
 * Read per call, not at module load, so one prebuilt bundle (`npx wheel-web`, the Docker image)
 * serves whatever its environment says when it starts.
 */

export const DEFAULT_API_URL = "http://127.0.0.1:8080";
const DEFAULT_PROXY_BODY_LIMIT = 5 * 1024 * 1024;
const AUTH_MODES: readonly AuthMode[] = ["mock", "dev", "local", "clerk"];

/** `WHEEL_API_URL`, else the `NEXT_PUBLIC_API_URL` an existing deployment already sets. */
export function serverApiBaseUrl(): string {
  // Truthiness, not `??`: `WHEEL_API_URL=` with nothing after it is ordinary in a shell script or
  // a compose file, and an empty base would send every request to a relative path.
  const value = firstNonEmpty(process.env.WHEEL_API_URL, process.env.NEXT_PUBLIC_API_URL) ?? DEFAULT_API_URL;
  let parsed: URL | null = null;
  try {
    parsed = new URL(value);
  } catch {
    /* reported below */
  }
  if (!parsed || (parsed.protocol !== "http:" && parsed.protocol !== "https:")) {
    throw new Error(`WHEEL_API_URL must be an http(s) URL, got ${JSON.stringify(value)}.`);
  }
  return `${parsed.origin}${parsed.pathname.replace(/\/+$/, "")}`;
}

/** `WHEEL_AUTH_MODE`, else `NEXT_PUBLIC_AUTH_MODE` for deployments configured before it existed. */
export function serverAuthMode(): AuthMode {
  return parseAuthMode(firstNonEmpty(process.env.WHEEL_AUTH_MODE, process.env.NEXT_PUBLIC_AUTH_MODE));
}

/**
 * Unset means `mock`, as it always has. A value that is set but unrecognised throws: a typo here
 * used to fail silently as "sign-in works, then everything 401s", and a server that refuses to
 * render with the reason in its log is the cheaper failure.
 */
export function parseAuthMode(raw: string | undefined): AuthMode {
  if (raw === undefined) return "mock";
  const mode = raw.trim();
  if ((AUTH_MODES as readonly string[]).includes(mode)) return mode as AuthMode;
  throw new Error(`WHEEL_AUTH_MODE=${JSON.stringify(raw)} is not one of: ${AUTH_MODES.join(", ")}.`);
}

/**
 * `WHEEL_PUBLIC_ORIGIN`: the origin browsers use to reach this app (`https://wheel.example.com`),
 * for when a TLS-terminating proxy stands in front and this server cannot see it. When set it is
 * the whole answer: forwarded headers are ignored entirely.
 */
export function publicOriginSetting(): string | null {
  const value = firstNonEmpty(process.env.WHEEL_PUBLIC_ORIGIN);
  if (!value) return null;
  let parsed: URL | null = null;
  try {
    parsed = new URL(value);
  } catch {
    /* reported below */
  }
  if (!parsed || !/^https?:$/.test(parsed.protocol) || parsed.pathname !== "/" || parsed.search || parsed.hash) {
    throw new Error(`WHEEL_PUBLIC_ORIGIN must be an origin like https://wheel.example.com, got ${JSON.stringify(value)}.`);
  }
  return parsed.origin;
}

/**
 * Whether X-Forwarded-Proto and X-Forwarded-Host describe the public origin. Only a proxy that
 * overwrites them may be trusted, and this server cannot see who connected, so it is declared:
 * `WHEEL_TRUST_PROXY=1`, or on Vercel, whose edge always sets them.
 */
export function trustProxy(): boolean {
  const value = firstNonEmpty(process.env.WHEEL_TRUST_PROXY)?.toLowerCase();
  if (value !== undefined) return value === "1" || value === "true";
  return process.env.VERCEL === "1";
}

/** What dev and mock modes present to the API. Server-only by construction: never NEXT_PUBLIC_. */
export function devToken(): string | null {
  return firstNonEmpty(process.env.WHEEL_DEV_TOKEN) ?? null;
}

/** Bodies over this are refused before they are buffered. Keep it at the API's INGRESS_BODY_LIMIT_BYTES. */
export function proxyBodyLimit(): number {
  const n = Number(process.env.WHEEL_PROXY_BODY_LIMIT_BYTES);
  return Number.isSafeInteger(n) && n > 0 ? n : DEFAULT_PROXY_BODY_LIMIT;
}

function firstNonEmpty(...values: (string | undefined)[]): string | undefined {
  for (const value of values) if (typeof value === "string" && value.trim()) return value.trim();
  return undefined;
}
