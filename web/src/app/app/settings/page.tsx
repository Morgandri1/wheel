"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { Button, Field, Input } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";
import { AUTH_MODE } from "@/lib/auth";
import { changePassword, useSession } from "@/lib/local-auth";
import { passwordChangeProblem } from "@/lib/password";

export default function SettingsPage() {
  const session = useSession();
  const router = useRouter();
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [saving, setSaving] = useState(false);

  const problem = passwordChangeProblem(current, next, confirm);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (problem) return;
    setSaving(true);
    try {
      await changePassword(current, next);
      // The API revoked every session, this one included, so there is nothing left to stay on.
      toast("Password changed. Sign in again with your new password.");
      router.replace("/sign-in");
    } catch (err) {
      toastError(err, "Couldn't change your password.");
      setSaving(false);
    }
  };

  return (
    <main className="mx-auto flex w-full max-w-lg flex-col gap-6 p-8">
      <div>
        <h1 className="text-lead text-ink">Settings</h1>
        <p className="mt-1 text-meta text-ink-dim">
          {session.status === "authed" ? session.user.email : "Not signed in."}
        </p>
      </div>

      {AUTH_MODE === "local" ? (
        <form
          method="post"
          onSubmit={submit}
          className="flex flex-col gap-3"
          data-testid="form-change-password"
        >
          <h2 className="text-meta text-ink">Change password</h2>
          <Field label="Current password" hint="Required even though you are signed in: a stolen session cannot become a permanent takeover.">
            <Input
              type="password"
              autoComplete="current-password"
              value={current}
              onChange={(e) => setCurrent(e.target.value)}
              data-testid="input-current-password"
            />
          </Field>
          <Field label="New password">
            <Input
              type="password"
              autoComplete="new-password"
              value={next}
              onChange={(e) => setNext(e.target.value)}
              data-testid="input-new-password"
            />
          </Field>
          <Field label="Confirm new password">
            <Input
              type="password"
              autoComplete="new-password"
              value={confirm}
              onChange={(e) => setConfirm(e.target.value)}
              data-testid="input-confirm-password"
            />
          </Field>

          <p className="text-micro text-ink-faint">
            Changing your password signs out every session, including this one — that is the point,
            so a leaked password cannot leave a leaked session alive. You will sign in again.
          </p>

          {problem && (current || next || confirm) ? (
            <p className="text-micro text-[var(--danger)]" data-testid="password-problem">
              {problem}
            </p>
          ) : null}

          <Button type="submit" disabled={!!problem || saving} data-testid="btn-change-password">
            {saving ? "Changing…" : "Change password"}
          </Button>
        </form>
      ) : (
        <p className="text-meta text-ink-dim" data-testid="password-not-local">
          This deployment signs in through an external provider, so your password is managed there.
        </p>
      )}

      <div className="border-t border-rule pt-4">
        <h2 className="text-meta text-ink">Forgotten password</h2>
        <p className="mt-1 text-micro text-ink-faint" data-testid="reset-unavailable">
          There is no email-based reset yet — it needs a mail provider and there is none deployed
          (M3). If you cannot sign in, an operator has to reset it for you. Saying so here beats a
          &ldquo;reset&rdquo; link that quietly goes nowhere.
        </p>
      </div>
    </main>
  );
}
