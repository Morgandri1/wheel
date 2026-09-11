// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";
import type { AuthMode } from "@/lib/auth";

/**
 * Server configuration, read per call on the SERVER and never shipped to the browser, so one
 * prebuilt bundle follows the environment it starts in. What each setting means for the trust
 * model is written down once, in web/DEPLOY.md ("The trust model").
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
  return refuseSharedCredentialInProduction(
    parseAuthMode(firstNonEmpty(process.env.WHEEL_AUTH_MODE, process.env.NEXT_PUBLIC_AUTH_MODE)),
  );
}

/**
 * Unset means `mock`, as it always has. A value that is set but unrecognised throws: a typo here
 * used to fail silently as "sign-in works, then everything 401s".
 */
export function parseAuthMode(raw: string | undefined): AuthMode {
  if (raw === undefined) return "mock";
  const mode = raw.trim();
  if ((AUTH_MODES as readonly string[]).includes(mode)) return mode as AuthMode;
  throw new Error(`WHEEL_AUTH_MODE=${JSON.stringify(raw)} is not one of: ${AUTH_MODES.join(", ")}.`);
}

/**
 * mock and dev present one credential for every visitor, so in production either makes this
 * server an open door to the API — and mock is what an UNSET WHEEL_AUTH_MODE means. Refused
 * unless WHEEL_ALLOW_INSECURE_AUTH=1 says an open server is meant.
 */
export function refuseSharedCredentialInProduction(mode: AuthMode): AuthMode {
  const shared = mode === "mock" || mode === "dev";
  if (shared && process.env.NODE_ENV === "production" && process.env.WHEEL_ALLOW_INSECURE_AUTH !== "1") {
    throw new Error(
      `WHEEL_AUTH_MODE=${mode} (the default when unset) would let every visitor of this production server act with one shared credential. Set WHEEL_AUTH_MODE=local or clerk, or WHEEL_ALLOW_INSECURE_AUTH=1 if an open server is really meant.`,
    );
  }
  return mode;
}

/** `WHEEL_PUBLIC_ORIGIN`: the origin browsers use to reach this app, when a proxy stands in front. */
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

/** `WHEEL_TRUST_PROXY=1`, or Vercel, whose edge always sets the forwarded headers. */
export function trustProxy(): boolean {
  const value = firstNonEmpty(process.env.WHEEL_TRUST_PROXY)?.toLowerCase();
  if (value !== undefined) return value === "1" || value === "true";
  return process.env.VERCEL === "1";
}

/** What dev mode presents to the API. Server-only by construction: never NEXT_PUBLIC_. */
export function devToken(): string | null {
  return firstNonEmpty(process.env.WHEEL_DEV_TOKEN) ?? null;
}

/** Bodies over this are refused. Keep it at the API's INGRESS_BODY_LIMIT_BYTES. */
export function proxyBodyLimit(): number {
  const n = Number(process.env.WHEEL_PROXY_BODY_LIMIT_BYTES);
  return Number.isSafeInteger(n) && n > 0 ? n : DEFAULT_PROXY_BODY_LIMIT;
}

/**
 * Every setting validated at once, for the startup line (src/instrumentation.ts): a server that
 * would refuse every request should fail to start, not come up looking healthy.
 */
export function checkServerConfig(): string {
  const api = serverApiBaseUrl();
  const mode = serverAuthMode();
  const origin = publicOriginSetting();
  const answers = origin ?? (trustProxy() ? "whatever a trusted proxy reports" : "localhost only");
  const defaulted = firstNonEmpty(process.env.WHEEL_API_URL, process.env.NEXT_PUBLIC_API_URL) ? "" : " (WHEEL_API_URL unset)";
  return `API ${api}${defaulted} · auth ${mode} · public origin: ${answers}`;
}

function firstNonEmpty(...values: (string | undefined)[]): string | undefined {
  for (const value of values) if (typeof value === "string" && value.trim()) return value.trim();
  return undefined;
}
