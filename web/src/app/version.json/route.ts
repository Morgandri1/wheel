import { NextResponse } from "next/server";
import pkg from "../../../package.json";

/**
 * What build is actually live, answerable without logging in.
 *
 * Every feature ships behind the login wall, so "is the new build deployed?" was unanswerable from
 * outside: a diff of 0.2.1 to 0.3.0 changed ZERO public-route files, and four different attempts to
 * read it off the served chunks failed because the board's chunk hash is never handed to a
 * logged-out client. The answer was "log in and look at a panel", which is not a check anyone can
 * automate and cost real time to reach.
 *
 * `commit` is Vercel's own build-time variable, so the answer names the exact source, not just a
 * version someone may have forgotten to bump.
 */
export const dynamic = "force-static";

export function GET() {
  return NextResponse.json(
    {
      version: pkg.version,
      commit: process.env.VERCEL_GIT_COMMIT_SHA ?? null,
      built_at: new Date().toISOString(),
    },
    // Never cached: a stale answer to "what is deployed" is worse than no answer, because it is
    // indistinguishable from a correct one.
    { headers: { "cache-control": "no-store" } },
  );
}
