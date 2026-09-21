// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { clerkMiddleware, createRouteMatcher } from "@clerk/nextjs/server";
import { NextResponse, type NextFetchEvent, type NextMiddleware, type NextRequest } from "next/server";
import { serverAuthMode } from "@/lib/runtime-config";
import { publicOrigin } from "@/lib/same-origin";
import { buildCsp } from "@/lib/csp";
import { liveSessionToken, signInRedirect } from "@/lib/session-cookie";

/**
 * Three jobs: the Content Security Policy on every response, Clerk's route guard in clerk mode,
 * and sending a visitor with no session cookie from /app to sign-in in local mode.
 *
 * Clerk guards /app/* — and guarding means REQUIRING a session, not merely making one available.
 * `clerkMiddleware()` on its own only populates auth; without the protect() call below an
 * unauthenticated visitor reaches the board and only finds out when the API 401s. It also has to
 * run on /api/*, or `auth()` in the route handlers has no session to read.
 *
 * The local-mode redirect looks only for a cookie shaped like a live session. That is a routing
 * courtesy, not a boundary: a revoked session still reaches the page, and the API refuses it there.
 */
const isProtected = createRouteMatcher(["/app", "/app/(.*)"]);

/**
 * `/api/wheel/*` forwards upstream bytes unchanged, and for anything that is not JSON,
 * `proxy-rules.ts`'s `returnedResponseHeaders` sets `content-security-policy: sandbox` — the lock
 * that keeps a chest blob or an SSE frame from running as a document with the user's session, if
 * someone opens its raw URL directly. Next applies middleware's response headers LAST, after the
 * route handler's, so unconditionally setting the document CSP here would silently replace that
 * `sandbox` with the page policy and undo the lock. Nothing under this prefix is ever a page, so
 * the document CSP has no job here and is left to whatever the route itself set.
 */
const PROXIED_API_PREFIX = "/api/wheel/";

let clerk: NextMiddleware | null = null;
const clerkGuard = (): NextMiddleware =>
  (clerk ??= clerkMiddleware(async (auth, req) => {
    if (isProtected(req)) await auth.protect();
  }));

export default async function middleware(req: NextRequest, ev: NextFetchEvent) {
  const mode = serverAuthMode();
  // A fresh nonce per request. Reusing one across responses would make it forgeable by anyone
  // who has seen a single page.
  const nonce = Buffer.from(crypto.randomUUID()).toString("base64");
  const csp = buildCsp({ nonce, authMode: mode, dev: process.env.NODE_ENV !== "production" });

  if (mode === "local") {
    const target = signInRedirect(req.nextUrl.pathname, liveSessionToken(req) !== null);
    if (target) {
      // The base is the origin browsers use, not `req.url`: behind a proxy Next's standalone builds
      // `req.url` from its own bind address (HOSTNAME/PORT), which sent browsers to
      // https://localhost:3000. `publicOrigin` is the derivation every /api route already uses: a
      // configured WHEEL_PUBLIC_ORIGIN wins outright, and forwarded headers count only under
      // WHEEL_TRUST_PROXY, so a client cannot steer it. `"null"` means it cannot be told, which is
      // only reachable with neither setting — the localhost-only mode, where `req.url` is right.
      // (A relative Location does not work here: Next's middleware adapter parses it as absolute
      // and answers 500.) `target` is always an app-built `/sign-in[?next=<encoded /app path>]`.
      const origin = publicOrigin(req);
      const redirect = NextResponse.redirect(new URL(target, origin === "null" ? req.url : origin));
      redirect.headers.set("content-security-policy", csp);
      return redirect;
    }
  }

  // Next reads the policy off the REQUEST headers to nonce its own bootstrap scripts; the
  // response header is what the browser enforces. Both are required.
  const headers = new Headers(req.headers);
  headers.set("x-nonce", nonce);
  headers.set("content-security-policy", csp);

  const res = mode === "clerk" ? await clerkGuard()(req, ev) : NextResponse.next({ request: { headers } });
  const out = res instanceof NextResponse ? res : NextResponse.next({ request: { headers } });
  if (!req.nextUrl.pathname.startsWith(PROXIED_API_PREFIX)) {
    out.headers.set("content-security-policy", csp);
  }
  return out;
}

export const config = {
  // Skip Next internals and static files; run on everything else so Clerk can see the session
  // and every document response carries the policy.
  matcher: ["/((?!_next|[^?]*\\.(?:html?|css|js|jpe?g|png|svg|webp|ico|woff2?)).*)", "/(api|trpc)(.*)"],
};
