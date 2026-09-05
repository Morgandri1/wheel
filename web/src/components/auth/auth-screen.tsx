"use client";

import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { useEffect, useRef, useState } from "react";
import { AUTH_MODE } from "@/lib/auth";
import {
  MIN_PASSWORD_LENGTH,
  emailProblem,
  hydrateSession,
  passwordProblem,
  signIn,
  signUp,
  useSession,
} from "@/lib/local-auth";
import { WheelMark } from "@/components/header";
import { Button, Field, Input } from "@/components/ui";

/**
 * Sign in and sign up, for NEXT_PUBLIC_AUTH_MODE=local.
 *
 * One component for both because they are the same form with a different verb; splitting them
 * duplicates every error path and then they drift. The difference is three strings and whether
 * the password rule is enforced before the request (on sign-up it is a rule the user must meet;
 * on sign-in it is a fact about an existing password and enforcing it would lock out anyone
 * whose account predates the rule).
 */
export function AuthScreen({ mode }: { mode: "sign-in" | "sign-up" }) {
  const router = useRouter();
  const params = useSearchParams();
  const session = useSession();
  const isSignUp = mode === "sign-up";

  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [fieldError, setFieldError] = useState<{ email?: string; password?: string }>({});
  const [busy, setBusy] = useState(false);
  const emailRef = useRef<HTMLInputElement>(null);

  // `next` is where the user was headed before we intercepted them. Same-origin paths only —
  // an open redirect is exactly the kind of thing a sign-in page gets used for.
  const raw = params.get("next");
  const next = raw && raw.startsWith("/") && !raw.startsWith("//") ? raw : "/app";

  useEffect(() => {
    hydrateSession();
  }, []);

  useEffect(() => {
    emailRef.current?.focus();
  }, []);

  useEffect(() => {
    if (session.status === "authed") router.replace(next);
  }, [session.status, next, router]);

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (busy) return;

    const problems = {
      email: emailProblem(email) ?? undefined,
      password: (isSignUp ? passwordProblem(password) : password ? null : "Enter your password.") ?? undefined,
    };
    setFieldError(problems);
    if (problems.email || problems.password) return;

    setError(null);
    setBusy(true);
    try {
      if (isSignUp) await signUp(email, password);
      else await signIn(email, password);
      router.replace(next);
    } catch (e) {
      setError((e as Error).message || "That didn't work.");
      setPassword("");
      setBusy(false);
    }
  };

  if (AUTH_MODE !== "local") {
    return (
      <div className="plate max-w-sm p-5 text-meta text-ink-dim" data-testid="auth-wrong-mode">
        This build is running <span className="ident">{AUTH_MODE}</span> auth, so it has no
        email and password form. Set <span className="ident">NEXT_PUBLIC_AUTH_MODE=local</span> to
        use one.
      </div>
    );
  }

  return (
    <div className="w-full max-w-[24rem]" data-testid="auth-screen" data-mode={mode}>
      <div className="plate">
        {/* The one red the system allows, used here the way it is used on a node card: as an edge. */}
        <div className="h-1" style={{ background: "var(--accent)" }} />

        <div className="flex items-center gap-2 border-b-2 border-[var(--rule-strong)] px-5 py-3">
          <Link href="/" className="flex items-center gap-2 text-ink" data-testid="link-home">
            <WheelMark />
            <span className="display text-meta font-semibold tracking-tight">wheel</span>
          </Link>
        </div>

        <div className="px-5 py-6">
          <h1 className="display text-[1.75rem] leading-none">
            {isSignUp ? "Make an account" : "Sign in"}
          </h1>
          <p className="mb-6 mt-1.5 text-meta text-ink-dim">
            {isSignUp ? "One account, every board you build." : "Your boards are where you left them."}
          </p>

          <form onSubmit={submit} className="flex flex-col gap-4" noValidate data-testid="auth-form">
            <Field label="Email" htmlFor="auth-email" error={fieldError.email}>
              <Input
                id="auth-email"
                ref={emailRef}
                type="email"
                name="email"
                autoComplete="email"
                autoCapitalize="off"
                spellCheck={false}
                placeholder="you@example.com"
                data-testid="input-email"
                value={email}
                onChange={(e) => {
                  setEmail(e.target.value);
                  if (fieldError.email) setFieldError((f) => ({ ...f, email: undefined }));
                }}
              />
            </Field>

            <Field
              label="Password"
              htmlFor="auth-password"
              error={fieldError.password}
              hint={isSignUp ? `At least ${MIN_PASSWORD_LENGTH} characters.` : undefined}
            >
              <Input
                id="auth-password"
                type="password"
                name="password"
                autoComplete={isSignUp ? "new-password" : "current-password"}
                data-testid="input-password"
                value={password}
                onChange={(e) => {
                  setPassword(e.target.value);
                  if (fieldError.password) setFieldError((f) => ({ ...f, password: undefined }));
                }}
              />
            </Field>

            {error ? (
              <p
                className="border-l-2 border-[var(--danger)] pl-2.5 text-meta"
                style={{ color: "var(--danger)" }}
                role="alert"
                data-testid="auth-error"
              >
                {error}
              </p>
            ) : null}

            <Button type="submit" tone="primary" disabled={busy} data-testid="btn-auth-submit">
              {busy ? (isSignUp ? "Creating\u2026" : "Signing in\u2026") : isSignUp ? "Create account" : "Sign in"}
            </Button>
          </form>
        </div>

        <div className="border-t border-rule px-5 py-3 text-meta text-ink-dim">
          {isSignUp ? "Already have an account? " : "No account yet? "}
          <Link
            href={isSignUp ? "/sign-in" : "/sign-up"}
            className="text-ink underline underline-offset-4"
            data-testid="link-auth-switch"
          >
            {isSignUp ? "Sign in" : "Make one"}
          </Link>
        </div>
      </div>
    </div>
  );
}
