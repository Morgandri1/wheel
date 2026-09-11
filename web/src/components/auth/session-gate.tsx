"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { usePathname, useRouter } from "next/navigation";
import { useEffect } from "react";
import { authMode } from "@/lib/auth";
import { hydrateSession, useSession } from "@/lib/local-auth";

/**
 * Guards /app in local auth mode, behind middleware's cookie check.
 *
 * Middleware can only see that a cookie is present; this asks the server whether it is still
 * alive, and sends a visitor whose session has died to sign in. Both are routing courtesies, not
 * a security boundary: the boundary is the API, which refuses every request without a valid
 * session and 404s projects you do not own.
 *
 * The three states are deliberately distinct. `loading` means the server has not answered yet,
 * and redirecting during it would sign out every returning user for one frame.
 */
export function SessionGate({ children }: { children: React.ReactNode }) {
  const session = useSession();
  const router = useRouter();
  const pathname = usePathname();
  const local = authMode() === "local";

  useEffect(() => {
    if (local) void hydrateSession();
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
