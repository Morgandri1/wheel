import { clerkMiddleware } from "@clerk/nextjs/server";
import { NextResponse, type NextRequest } from "next/server";
import type { NextFetchEvent } from "next/server";

/**
 * Clerk guards /app/* only when it is configured. In mock auth mode the middleware is a no-op,
 * so the whole board runs locally without a Clerk instance.
 */
const clerk = process.env.NEXT_PUBLIC_AUTH_MODE === "clerk" ? clerkMiddleware() : null;

export default function middleware(req: NextRequest, ev: NextFetchEvent) {
  if (clerk) return clerk(req, ev);
  return NextResponse.next();
}

export const config = {
  matcher: ["/app", "/app/(.*)"],
};
