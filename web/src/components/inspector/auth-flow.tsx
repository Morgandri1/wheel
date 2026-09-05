"use client";

import { useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Button, Field, Input } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";
import type { EngineApi } from "@/lib/api";

/**
 * Signing an agent in.
 *
 * An API key is the live path today (PROTOCOL.md §auth: POST auth/complete {api_key}), so it is
 * what this panel leads with. OAuth needs `auth/begin` to hold a live child process open for the
 * device or paste-code exchange; that lands in M2, and its buttons are present-but-disabled with
 * the reason rather than hidden — an absent control is something people hunt for.
 *
 * The key is write-only in the strongest sense available: it is posted and the field is cleared,
 * it is never read back (no route returns it), and it is never put in a URL, a query key or a
 * toast. What comes back is whether the agent is authenticated, and optionally which account.
 */
export function AuthFlow({
  api,
  nodeId,
  needsAuth,
  onAuthenticated,
}: {
  api: EngineApi;
  nodeId: string;
  /** The agent tried to start and had no credentials — the one moment this panel is urgent. */
  needsAuth: boolean;
  onAuthenticated: () => void;
}) {
  const agent = api.agent(nodeId);
  const [apiKey, setApiKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [justSaved, setJustSaved] = useState(false);
  const [replacing, setReplacing] = useState(false);
  const keyRef = useRef<HTMLInputElement>(null);

  const status = useQuery({
    queryKey: ["auth", nodeId],
    queryFn: () => agent.authStatus(),
    // Poll only while a submission is settling. The engine writes credentials to disk and probes
    // the harness, so "authenticated" can land a beat after the request returns.
    refetchInterval: justSaved ? 2000 : false,
  });

  const authenticated = status.data?.authenticated ?? false;

  useEffect(() => {
    if (authenticated && justSaved) {
      setJustSaved(false);
      onAuthenticated();
    }
  }, [authenticated, justSaved, onAuthenticated]);

  useEffect(() => {
    if (needsAuth && !authenticated) keyRef.current?.focus();
  }, [needsAuth, authenticated]);

  const save = async () => {
    const key = apiKey.trim();
    if (!key) return;
    setBusy(true);
    try {
      const next = await agent.authComplete({ api_key: key });
      // Clear immediately: the key has left, and there is no reason for it to sit in a form.
      setApiKey("");
      setReplacing(false);
      setJustSaved(true);
      if (next.authenticated) {
        toast("Agent authenticated. Start it to pick up any queued messages.");
        onAuthenticated();
      }
      await status.refetch();
    } catch (e) {
      toastError(e, "The engine would not accept that key.");
    } finally {
      setBusy(false);
    }
  };

  if (authenticated && !replacing) {
    return (
      <div
        className="flex items-center justify-between gap-2 border border-rule px-2.5 py-2"
        data-testid="auth-status"
        data-authenticated="true"
      >
        <span className="text-micro text-ink-dim">
          Authenticated
          {status.data?.account ? ` · ${status.data.account}` : " · API key"}
        </span>
        <Button size="sm" tone="ghost" data-testid="btn-auth-replace" onClick={() => setReplacing(true)}>
          Replace
        </Button>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-3 border border-rule p-2.5" data-testid="auth-flow">
      {needsAuth && !authenticated ? (
        <p
          data-testid="auth-needs-auth-callout"
          className="text-micro"
          style={{ color: "var(--danger)" }}
        >
          This agent has no credentials, so it stopped at startup. Anything sent to it is queued
          and will be delivered once it is authenticated and restarted.
        </p>
      ) : null}

      <Field
        label={replacing ? "New API key" : "API key"}
        hint={
          replacing
            ? "Replaces the key already stored for this agent."
            : "Stored in the agent's own credentials directory. Never shown again, by anything."
        }
      >
        <Input
          ref={keyRef}
          type="password"
          mono
          autoComplete="off"
          spellCheck={false}
          data-testid="input-api-key"
          placeholder="sk-…"
          value={apiKey}
          onChange={(e) => setApiKey(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void save();
          }}
        />
      </Field>

      <div className="flex items-center gap-2">
        <Button
          tone="primary"
          size="sm"
          data-testid="btn-auth-complete"
          disabled={!apiKey.trim() || busy}
          onClick={save}
        >
          {busy ? "Checking…" : "Save key"}
        </Button>
        {replacing ? (
          <Button size="sm" tone="ghost" onClick={() => { setReplacing(false); setApiKey(""); }}>
            Cancel
          </Button>
        ) : null}
        {justSaved && !authenticated ? (
          <span className="text-micro text-ink-faint" data-testid="auth-checking">
            Waiting for the harness to confirm…
          </span>
        ) : null}
      </div>

      <div className="flex items-center gap-2 border-t border-rule pt-2.5">
        <Button
          size="sm"
          disabled
          data-testid="btn-auth-oauth"
          title="Signing in with your own Claude or Codex account arrives with the engine's OAuth flow (M2)."
        >
          Sign in with your account
        </Button>
        <span className="text-micro text-ink-faint">M2 — uses your own plan instead of a key</span>
      </div>
    </div>
  );
}
