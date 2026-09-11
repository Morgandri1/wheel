// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";
import { errorEnvelope } from "@/lib/proxy-rules";
import { publicOriginSetting, trustProxy } from "@/lib/runtime-config";

/**
 * The CSRF and DNS-rebinding checks every /api route runs first. The rules and their reasons are
 * written down once, in web/DEPLOY.md ("The trust model"); this file is their implementation.
 */

const SAFE_METHODS = new Set(["GET", "HEAD", "OPTIONS"]);

export interface ProxyTrust {
  publicOrigin: string | null;
  trustProxy: boolean;
}

export function proxyTrust(): ProxyTrust {
  return { publicOrigin: publicOriginSetting(), trustProxy: trustProxy() };
}

/** The origin a browser uses to reach this app, e.g. `https://wheel.example.com`; `"null"` if it cannot be told. */
export function publicOrigin(req: Request, trust: ProxyTrust = proxyTrust()): string {
  if (trust.publicOrigin) return trust.publicOrigin;
  const proto = trust.trustProxy ? lastValue(req.headers.get("x-forwarded-proto")) : null;
  const forwardedHost = trust.trustProxy ? lastValue(req.headers.get("x-forwarded-host")) : null;
  try {
    const url = new URL(req.url);
    const host = forwardedHost ?? req.headers.get("host") ?? url.host;
    return new URL(`${proto ? `${proto}:` : connectionProtocol(req, url)}//${host}`).origin;
  } catch {
    return "null";
  }
}

/**
 * The scheme of the connection this server accepted. Next fills X-Forwarded-Proto only when it is
 * absent and keeps a client's own, so that header — and the URL scheme Next derives from it — may
 * be the client's words. Untrusted, they mean nothing, and this server never terminates TLS itself.
 */
function connectionProtocol(req: Request, url: URL): string {
  return req.headers.has("x-forwarded-proto") ? "http:" : url.protocol;
}

export function isLoopbackHost(host: string): boolean {
  let hostname: string;
  try {
    hostname = new URL(`http://${host}`).hostname;
  } catch {
    return false;
  }
  return hostname === "localhost" || hostname === "[::1]" || /^127(?:\.\d{1,3}){3}$/.test(hostname);
}

/** With no public origin and no trusted proxy, any Host but loopback may be a DNS-rebound name. */
export function hostAllowed(req: Request, trust: ProxyTrust = proxyTrust()): boolean {
  if (trust.publicOrigin || trust.trustProxy) return true;
  return isLoopbackHost(req.headers.get("host") ?? new URL(req.url).host);
}

/**
 * The client's address as the nearest proxy recorded it. Only as good as that proxy: without a
 * trusted one, Next keeps whatever X-Forwarded-For the client sent, so this is the client's claim.
 */
export function clientAddress(req: Request): string | null {
  return lastValue(req.headers.get("x-forwarded-for"));
}

export interface RequestOrigin {
  method: string;
  origin: string | null;
  secFetchSite: string | null;
  publicOrigin: string;
}

export function isSameOrigin(r: RequestOrigin): boolean {
  if (r.secFetchSite === "cross-site") return false;
  if (SAFE_METHODS.has(r.method.toUpperCase())) return true;
  // When Origin is present it is the whole answer; Sec-Fetch-Site stands in only when it is absent.
  if (r.origin !== null) return r.publicOrigin !== "null" && originOf(r.origin) === r.publicOrigin;
  return r.secFetchSite === "same-origin";
}

function originOf(value: string): string | null {
  try {
    return new URL(value).origin;
  } catch {
    return null;
  }
}

export function requestOrigin(req: Request, trust: ProxyTrust = proxyTrust()): RequestOrigin {
  return {
    method: req.method,
    origin: req.headers.get("origin"),
    secFetchSite: req.headers.get("sec-fetch-site"),
    publicOrigin: publicOrigin(req, trust),
  };
}

/** A 403 for a request this app should not answer, or null to carry on. */
export function refuseCrossOrigin(req: Request): Response | null {
  const trust = proxyTrust();
  if (!hostAllowed(req, trust)) {
    return errorEnvelope(
      403,
      "public_origin_required",
      "This server answers only on localhost until WHEEL_PUBLIC_ORIGIN names the address browsers use to reach it.",
    );
  }
  const facts = requestOrigin(req, trust);
  if (isSameOrigin(facts)) return null;
  // The browser says the page is this site, yet its Origin is not the one computed here: a proxy
  // nobody told this server about, not an attack. Said where the operator looks.
  if (facts.origin && (facts.secFetchSite === "same-origin" || facts.secFetchSite === "same-site")) {
    console.warn(
      `wheel-web: refused ${facts.method} from Origin ${facts.origin}; this server believes its origin is ${facts.publicOrigin}. Behind a proxy, set WHEEL_PUBLIC_ORIGIN.`,
    );
  }
  return errorEnvelope(403, "cross_origin", "This request did not come from this app, so it was refused.");
}

/** The nearest proxy writes the rightmost value; anything to its left came from further out. */
function lastValue(header: string | null): string | null {
  const value = header?.split(",").at(-1)?.trim();
  return value ? value : null;
}
