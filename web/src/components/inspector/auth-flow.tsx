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
 * The credential is write-only in the strongest sense available: it is posted and the field is
 * cleared, it is never read back (no route returns it), and it is never put in a URL, a query key
 * or a toast. What comes back is whether a credential is STORED — which is not the same as it
 * working, and the copy here is careful to say only the true one.
 */
/**
 * What a stored credential is, in words, per `AuthStatus.mode`.
 *
 * The engine's CredentialKind can grow — "env" arrived with vault-provided tokens after this
 * file was written — so an unrecognised mode falls back to a truthful generic rather than
 * rendering nothing or crashing. A client that breaks when a server enum gains a member is a
 * client that forces lockstep deploys.
 */
function credentialLabel(mode: string | null | undefined, source: string | null | undefined, vaults: string[]): string {
  if (mode === "env") {
    // A token handed to the agent from a wired vault at spawn. Nobody typed it into this panel,
    // so "Replace" would be wrong — the value lives in the vault and is edited there.
    //
    // The engine names the vault in `source`; the wire list is only a fallback for an engine that
    // does not send it yet. Inference is a worse answer than a fact, and with one vault per
    // account (contract) an agent can legitimately be wired to several — so guessing from wires
    // can name the wrong one.
    if (source) return `Credentials from vault ${source}`;
    return vaults.length === 1
      ? `Credentials from vault ${vaults[0]}`
      : vaults.length > 1
        ? `Credentials from a wired vault — one of ${vaults.join(", ")}`
        : "Credentials from a wired vault";
  }
  if (mode === "api_key") return "Credentials saved · API key";
  if (mode === "oauth_token") return "Credentials saved · setup-token";
  if (mode === "oauth_session") return "Credentials saved · signed in";
  return "Credentials saved";
}

export function AuthFlow({
  api,
  nodeId,
  needsAuth,
  vaults = [],
  onAuthenticated,
}: {
  api: EngineApi;
  nodeId: string;
  /** The agent tried to start and had no credentials — the one moment this panel is urgent. */
  needsAuth: boolean;
  /** Names of vaults this agent has a read wire to, for the `env` mode hint. */
  vaults?: string[];
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
        toast("Credentials saved. Anything queued for this agent will be delivered.");
        onAuthenticated();
      }
      await status.refetch();
    } catch (e) {
      // The engine explains a rejected credential in words (wrong harness, empty value); say what it said.
      toastError(e, "The engine would not accept that credential.");
    } finally {
      setBusy(false);
    }
  };

  const authData = status.data as (typeof status.data & { source?: string | null }) | undefined;
  const mode = authData?.mode as string | null | undefined;
  const source = authData?.source;
  const fromVault = mode === "env";

  if (authenticated && !replacing) {
    return (
      <div
        className="flex items-center justify-between gap-2 border border-rule px-2.5 py-2"
        data-testid="auth-status"
        data-authenticated="true"
        data-mode={mode ?? ""}
        data-source={source ?? ""}
      >
        {/* SDK: `authenticated: true` means a credential is STORED, not that it works — only the
            harness's own probe can say that. "Connected" here would be a lie an expired token
            tells, and the support round-trip lands on us. */}
        <span className="text-micro text-ink-dim">
          {credentialLabel(mode, source, vaults)}
          {status.data?.account ? ` · ${status.data.account}` : ""}
        </span>
        {fromVault ? (
          // Nothing to replace here: the value lives in the vault node and is edited there.
          // Offering "Replace" would invite someone to shadow their own vault by hand.
          <span className="text-micro text-ink-faint" data-testid="auth-from-vault">
            edit it in the vault
          </span>
        ) : (
          <Button size="sm" tone="ghost" data-testid="btn-auth-replace" onClick={() => setReplacing(true)}>
            Replace
          </Button>
        )}
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
          This agent has no usable credentials, so it stopped at startup. Nothing sent to it was
          lost — anything queued stays queued and is delivered once a credential is saved.
        </p>
      ) : null}

      <Field
        label={replacing ? "New key or token" : "API key or setup-token"}
        hint={
          replacing
            ? "Replaces the credential already stored for this agent."
            : "An sk-ant- API key, or the token from `claude setup-token`. Stored in the agent's own credentials directory and never shown again, by anything."
        }
      >
        <Input
          ref={keyRef}
          type="password"
          mono
          autoComplete="off"
          spellCheck={false}
          data-testid="input-api-key"
          placeholder="sk-ant-… or a setup-token"
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
          {busy ? "Saving…" : "Save credential"}
        </Button>
        {replacing ? (
          <Button size="sm" tone="ghost" onClick={() => { setReplacing(false); setApiKey(""); }}>
            Cancel
          </Button>
        ) : null}
        {justSaved && !authenticated ? (
          <span className="text-micro text-ink-faint" data-testid="auth-checking">
            Saving…
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
