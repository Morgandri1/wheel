"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { usePathname, useRouter } from "next/navigation";
import { useEffect } from "react";
import { Button } from "@/components/ui";
import { authMode } from "@/lib/auth";
import { hydrateSession, retrySession, useSession } from "@/lib/local-auth";

/**
 * Guards /app in local auth mode, behind middleware's cookie check. Middleware can only see a
 * cookie shaped like a live session; this asks the server whether it is one. Both are routing
 * courtesies, not a boundary: the API is the boundary (web/DEPLOY.md, "The trust model").
 *
 * Only `anon` redirects. `loading` has not asked yet, `unreachable` could not get an answer, and
 * `error` is the server refusing outright (most often a deployment missing `WHEEL_PUBLIC_ORIGIN`)
 * — none of those are "signed out", and sending any of them to sign-in would either bounce a
 * returning user over a blip or loop them into a form that cannot fix a server misconfiguration.
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

  if (session.status === "unreachable") {
    return (
      <div
        className="flex min-h-screen flex-col items-center justify-center gap-3 text-micro text-ink-faint"
        data-testid="session-unreachable"
      >
        <p>Can&rsquo;t reach the server to check your session. Trying again…</p>
        <Button size="sm" tone="ghost" data-testid="btn-session-retry" onClick={() => void retrySession()}>
          Try now
        </Button>
      </div>
    );
  }

  if (session.status === "error") {
    return (
      <div
        className="flex min-h-screen flex-col items-center justify-center gap-3 text-micro text-ink-faint"
        data-testid="session-error"
      >
        <p>{session.message}</p>
        <Button size="sm" tone="ghost" data-testid="btn-session-retry" onClick={() => void retrySession()}>
          Try again
        </Button>
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
