import { clerkMiddleware, createRouteMatcher } from "@clerk/nextjs/server";
import { NextResponse, type NextFetchEvent, type NextRequest } from "next/server";
import { serverApiBaseUrl } from "@/lib/runtime-config";
import { buildCsp } from "@/lib/csp";

/**
 * Two jobs: the Content Security Policy on every response, and Clerk's route guard when Clerk is
 * the configured provider.
 *
 * Clerk guards /app/* — and guarding means REQUIRING a session, not merely making one available.
 * `clerkMiddleware()` on its own only populates auth; without the protect() call below an
 * unauthenticated visitor reaches the board and only finds out when the API 401s.
 *
 * In every other mode that guard is a no-op. Local mode cannot be guarded here at all: its
 * session lives in localStorage and the edge has no cookie to read, so /app is gated in the
 * browser by SessionGate instead. That gate is a routing courtesy either way — the boundary is
 * the API, which refuses anything without a valid x-auth-token.
 */
const isProtected = createRouteMatcher(["/app", "/app/(.*)"]);

const clerk =
  process.env.NEXT_PUBLIC_AUTH_MODE === "clerk"
    ? clerkMiddleware(async (auth, req) => {
        if (isProtected(req)) await auth.protect();
      })
    : null;

export default async function middleware(req: NextRequest, ev: NextFetchEvent) {
  // A fresh nonce per request. Reusing one across responses would make it forgeable by anyone
  // who has seen a single page.
  const nonce = Buffer.from(crypto.randomUUID()).toString("base64");
  const csp = buildCsp({
    nonce,
    apiUrl: serverApiBaseUrl(),
    authMode: process.env.NEXT_PUBLIC_AUTH_MODE,
    dev: process.env.NODE_ENV !== "production",
  });

  // Next reads the policy off the REQUEST headers to nonce its own bootstrap scripts; the
  // response header is what the browser enforces. Both are required.
  const headers = new Headers(req.headers);
  headers.set("x-nonce", nonce);
  headers.set("content-security-policy", csp);

  const res = clerk ? await clerk(req, ev) : NextResponse.next({ request: { headers } });
  const out = res instanceof NextResponse ? res : NextResponse.next({ request: { headers } });
  out.headers.set("content-security-policy", csp);
  return out;
}

export const config = {
  // Skip Next internals and static files; run on everything else so Clerk can see the session
  // and every document response carries the policy.
  matcher: ["/((?!_next|[^?]*\\.(?:html?|css|js|jpe?g|png|svg|webp|ico|woff2?)).*)", "/(api|trpc)(.*)"],
};
