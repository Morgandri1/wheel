// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { clerkMiddleware, createRouteMatcher } from "@clerk/nextjs/server";
import { NextResponse, type NextFetchEvent, type NextMiddleware, type NextRequest } from "next/server";
import { serverAuthMode } from "@/lib/runtime-config";
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
      const redirect = NextResponse.redirect(new URL(target, req.url));
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
  out.headers.set("content-security-policy", csp);
  return out;
}

export const config = {
  // Skip Next internals and static files; run on everything else so Clerk can see the session
  // and every document response carries the policy.
  matcher: ["/((?!_next|[^?]*\\.(?:html?|css|js|jpe?g|png|svg|webp|ico|woff2?)).*)", "/(api|trpc)(.*)"],
};
