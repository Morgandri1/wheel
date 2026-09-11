// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";
import { errorEnvelope } from "@/lib/proxy-rules";
import { publicOriginSetting, trustProxy } from "@/lib/runtime-config";

/**
 * CSRF: a state-changing request must come from a page on this app's PUBLIC origin.
 *
 * The session is a cookie, so the browser attaches it to any request aimed here, including one a
 * hostile page makes. SameSite=Lax already withholds it from cross-site POSTs; this is the second
 * lock, and the only one in dev and mock mode, where the server supplies the credential itself.
 * A page cannot forge `Origin` or `Sec-Fetch-Site`; a non-browser client can forge both, but it
 * carries no victim's cookie.
 *
 * The public origin is what the browser sees, which behind a TLS-terminating proxy is not what
 * this server sees. In order of authority: WHEEL_PUBLIC_ORIGIN; X-Forwarded-Proto and -Host, but
 * only from a proxy declared trusted; otherwise the connection's own scheme and Host. A forged
 * X-Forwarded-* from anyone else changes nothing.
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
  const proto = trust.trustProxy ? firstValue(req.headers.get("x-forwarded-proto")) : null;
  const forwardedHost = trust.trustProxy ? firstValue(req.headers.get("x-forwarded-host")) : null;
  try {
    const url = new URL(req.url);
    const host = forwardedHost ?? req.headers.get("host") ?? url.host;
    return new URL(`${proto ? `${proto}:` : connectionProtocol(req, url)}//${host}`).origin;
  } catch {
    return "null";
  }
}

/**
 * The scheme of the connection this server accepted. Next derives the request URL's scheme from
 * X-Forwarded-Proto whenever one is present, so with an untrusted one in play the URL cannot be
 * believed either — and this server never terminates TLS itself, so the connection is plain http.
 */
function connectionProtocol(req: Request, url: URL): string {
  return req.headers.has("x-forwarded-proto") ? "http:" : url.protocol;
}

export interface RequestOrigin {
  method: string;
  origin: string | null;
  secFetchSite: string | null;
  publicOrigin: string;
}

export function isSameOrigin(r: RequestOrigin): boolean {
  // Nothing legitimately calls these routes from another site, whatever the method.
  if (r.secFetchSite === "cross-site") return false;
  if (SAFE_METHODS.has(r.method.toUpperCase())) return true;
  // A browser sends Origin with every state-changing request it makes, and when it does that is
  // the whole answer. Sec-Fetch-Site stands in only for a request that carries no Origin.
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

/** A 403 for a request that did not come from this app, or null to carry on. */
export function refuseCrossOrigin(req: Request): Response | null {
  const facts = requestOrigin(req);
  if (isSameOrigin(facts)) return null;
  // The browser says the page is this site, yet its Origin is not the one this server computed:
  // that is a proxy nobody told this server about, not an attack. Say so where the operator looks.
  if (facts.origin && (facts.secFetchSite === "same-origin" || facts.secFetchSite === "same-site")) {
    console.warn(
      `wheel-web: refused ${facts.method} from Origin ${facts.origin}; this server believes its origin is ${facts.publicOrigin}. Behind a proxy, set WHEEL_PUBLIC_ORIGIN.`,
    );
  }
  return errorEnvelope(403, "cross_origin", "This request did not come from this app, so it was refused.");
}

function firstValue(header: string | null): string | null {
  const value = header?.split(",")[0]?.trim();
  return value ? value : null;
}
