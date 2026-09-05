"use client";

import { usePathname, useRouter } from "next/navigation";
import { useEffect } from "react";
import { AUTH_MODE } from "@/lib/auth";
import { hydrateSession, useSession } from "@/lib/local-auth";

/**
 * Guards /app in local auth mode.
 *
 * This runs in the browser rather than in middleware, because a local session lives in
 * localStorage and the server cannot see it — there is no cookie to read at the edge. That makes
 * this a routing courtesy, not a security boundary: the boundary is the API, which refuses every
 * request without a valid `x-auth-token` and 404s projects you do not own. The point here is that
 * a signed-out visitor lands on a sign-in page instead of on a board full of failed requests.
 *
 * The three states are deliberately distinct. `loading` means we have not looked in storage yet,
 * and redirecting during it would sign out every returning user for one frame.
 */
export function SessionGate({ children }: { children: React.ReactNode }) {
  const session = useSession();
  const router = useRouter();
  const pathname = usePathname();
  const local = AUTH_MODE === "local";

  useEffect(() => {
    if (local) hydrateSession();
  }, [local]);

  useEffect(() => {
    if (!local || session.status !== "anon") return;
    const next = pathname && pathname !== "/app" ? `?next=${encodeURIComponent(pathname)}` : "";
    router.replace(`/sign-in${next}`);
  }, [local, session.status, pathname, router]);

  if (!local) return <>{children}</>;

  if (session.status === "loading") {
    return (
      <div
        className="flex min-h-screen items-center justify-center text-micro text-ink-faint"
        data-testid="session-loading"
      >
        Checking your session…
      </div>
    );
  }

  if (session.status === "anon") {
    return (
      <div
        className="flex min-h-screen items-center justify-center text-micro text-ink-faint"
        data-testid="session-redirecting"
      >
        Taking you to sign in…
      </div>
    );
  }

  return <>{children}</>;
}
