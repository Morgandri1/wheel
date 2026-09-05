import { clerkMiddleware, createRouteMatcher } from "@clerk/nextjs/server";
import { NextResponse, type NextFetchEvent, type NextRequest } from "next/server";

/**
 * Clerk guards /app/* — and guarding means REQUIRING a session, not merely making one available.
 * `clerkMiddleware()` on its own only populates auth; without the protect() call below an
 * unauthenticated visitor reaches the board and only finds out when the API 401s.
 *
 * In mock/dev auth mode this is a no-op, so the board runs locally with no Clerk instance.
 */
const isProtected = createRouteMatcher(["/app", "/app/(.*)"]);

const clerk =
  process.env.NEXT_PUBLIC_AUTH_MODE === "clerk"
    ? clerkMiddleware(async (auth, req) => {
        if (isProtected(req)) await auth.protect();
      })
    : null;

export default function middleware(req: NextRequest, ev: NextFetchEvent) {
  if (clerk) return clerk(req, ev);
  return NextResponse.next();
}

export const config = {
  // Skip Next internals and static files; run on everything else so Clerk can see the session.
  matcher: ["/((?!_next|[^?]*\\.(?:html?|css|js|jpe?g|png|svg|webp|ico|woff2?)).*)", "/(api|trpc)(.*)"],
};
