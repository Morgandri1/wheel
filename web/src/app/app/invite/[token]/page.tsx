"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { use, useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { acceptInvite } from "@/lib/api";
import { ApiError } from "@/lib/auth";
import { Header } from "@/components/header";
import { Button } from "@/components/ui";

/**
 * Where a `wi_…` invite link lands. Under `/app`, so middleware's session guard sends a signed-out
 * visitor to `/sign-in?next=/app/invite/<token>` (and, if they need an account first, the
 * sign-in/sign-up toggle now carries `next` along) — by the time this component mounts, there is
 * always a session to redeem the invite against.
 *
 * Accepting is server-side idempotent and never lowers an existing tier (docs/API.md), so firing
 * once on mount rather than waiting for a click is safe even under a double-invoked effect; the
 * ref guard is only to avoid a wasted second call, not correctness.
 */
export default function AcceptInvitePage({ params }: { params: Promise<{ token: string }> }) {
  const { token } = use(params);
  const router = useRouter();
  const [state, setState] = useState<{ kind: "pending" } | { kind: "error"; message: string } | { kind: "done" }>({
    kind: "pending",
  });
  const fired = useRef(false);

  useEffect(() => {
    if (fired.current) return;
    fired.current = true;
    acceptInvite(token)
      .then(({ project_id }) => {
        setState({ kind: "done" });
        router.replace(`/app/${project_id}`);
      })
      .catch((e: unknown) => {
        // The API gives one indistinguishable answer for unknown, expired, revoked and exhausted
        // invites — a link is a credential, and the response must not say which links exist.
        const message =
          e instanceof ApiError ? e.message : "Couldn't reach this app's server. Check your connection.";
        setState({ kind: "error", message });
      });
    // `token` and `router` are stable for the life of this page; re-running on their identity
    // would only matter if the URL itself changed, which unmounts this component anyway.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="flex min-h-screen flex-col">
      <Header />
      <main className="mx-auto flex w-full max-w-md flex-1 flex-col items-center justify-center gap-4 p-8 text-center">
        {state.kind === "pending" ? (
          <p className="text-meta text-ink-dim" data-testid="invite-pending">
            Joining the project…
          </p>
        ) : state.kind === "error" ? (
          <>
            <p className="text-meta text-ink" data-testid="invite-error">
              Couldn&rsquo;t join: {state.message}
            </p>
            <Button onClick={() => router.push("/app")} data-testid="btn-invite-back">
              Go to your projects
            </Button>
          </>
        ) : (
          <p className="text-meta text-ink-dim" data-testid="invite-done">
            Joined. Taking you there…
          </p>
        )}
      </main>
    </div>
  );
}
