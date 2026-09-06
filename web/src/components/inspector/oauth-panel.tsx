"use client";

import { useEffect, useState } from "react";
import { Button, Field, Input, Select } from "@/components/ui";
import { countdown, completeFailure, expiryFrom, vaultShareNote } from "@/lib/auth-session";
import type { AuthBegin, AuthStatus } from "@/lib/schema";
import type { EngineApi } from "@/lib/api";

/**
 * Signing an agent in with a real Anthropic account, in the browser.
 *
 * The operator cannot always run `claude setup-token` — that is the whole reason this exists — so
 * every step happens here: the engine opens a login, we show its URL and instructions verbatim,
 * the person completes it in a new tab and pastes the code back.
 *
 * Two things this panel refuses to imply. A sign-in window expires (the countdown is shown, not
 * hidden), and a login shared into a vault is a SHORT-LIVED token rather than a durable
 * arrangement — `vaultShareNote` says so in the engine's own words.
 */

/** The engine sends a deadline the generated schema does not carry yet. */
type BeginResponse = AuthBegin & { expires_in?: number | null };
type CompleteResponse = AuthStatus & {
  saved_to_vault?: { name?: string; expires_at?: string | null; warning?: string | null } | null;
};

export function OauthPanel({
  api,
  nodeId,
  vaults,
  defaultVault,
  onAuthenticated,
  onShareNote,
}: {
  api: EngineApi;
  nodeId: string;
  /** Vaults this agent has a read wire to — the only places a login may be shared. */
  vaults: string[];
  /**
   * Preselected share target. Signing in again on an agent whose credential already comes FROM a
   * vault almost always means replacing that vault's value, not shadowing it with a private copy.
   */
  defaultVault?: string;
  onAuthenticated: () => void;
  /**
   * Hand the vault warning to the parent. A successful sign-in unmounts this panel in favour of
   * the authenticated view, so a note kept in local state would disappear at the exact moment it
   * becomes true — which is how someone ends up believing a short-lived token is permanent.
   */
  onShareNote?: (note: string | null) => void;
}) {
  const agent = api.agent(nodeId);
  const [begun, setBegun] = useState<BeginResponse | null>(null);
  const [expiresAt, setExpiresAt] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [code, setCode] = useState("");
  const [vault, setVault] = useState(defaultVault ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expired, setExpired] = useState(false);
  const [shareNote, setShareNote] = useState<string | null>(null);

  // One second is the coarsest tick that still renders a truthful m:ss, and it only runs while a
  // sign-in is actually open — an idle panel costs nothing.
  useEffect(() => {
    if (expiresAt === null) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [expiresAt]);

  const remaining = countdown(expiresAt, now);
  const windowClosed = expiresAt !== null && remaining === null;

  const begin = async () => {
    setBusy(true);
    setError(null);
    setExpired(false);
    setShareNote(null);
    try {
      const res = (await agent.authBegin({ mode: "paste_code" })) as BeginResponse;
      setBegun(res);
      setExpiresAt(expiryFrom(res.expires_in, Date.now()));
      setNow(Date.now());
      setCode("");
    } catch (e) {
      setError(completeFailure(e).message);
    } finally {
      setBusy(false);
    }
  };

  const submit = async () => {
    const value = code.trim();
    if (!value || !begun) return;
    setBusy(true);
    setError(null);
    try {
      const res = (await agent.authComplete({
        code: value,
        session: begun.session,
        ...(vault ? { save_to_vault: vault } : {}),
      })) as CompleteResponse;
      setCode("");
      const note = vault ? vaultShareNote(res.saved_to_vault ?? {}) : null;
      setShareNote(note);
      onShareNote?.(note);
      if (res.authenticated) {
        setBegun(null);
        setExpiresAt(null);
        onAuthenticated();
      }
    } catch (e) {
      const failure = completeFailure(e);
      setError(failure.message);
      // An expired session cannot be retyped out of; the only way forward is a new code.
      if (failure.kind === "expired") setExpired(true);
    } finally {
      setBusy(false);
    }
  };

  if (!begun) {
    return (
      <div className="flex flex-col gap-2 border-t border-rule pt-2.5">
        <div className="flex items-center gap-2">
          <Button size="sm" tone="primary" data-testid="btn-auth-oauth" disabled={busy} onClick={begin}>
            {busy ? "Opening…" : "Sign in with Anthropic"}
          </Button>
          <span className="text-micro text-ink-faint">uses your own plan instead of a key</span>
        </div>
        {error ? (
          <p className="text-micro" style={{ color: "var(--danger)" }} data-testid="auth-error">
            {error}
          </p>
        ) : null}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-2.5 border-t border-rule pt-2.5" data-testid="auth-paste-code">
      <p className="text-micro text-ink-dim">{begun.instructions}</p>

      {begun.url ? (
        <a
          href={begun.url}
          target="_blank"
          rel="noopener noreferrer"
          className="ident text-micro underline"
          data-testid="auth-link"
        >
          {begun.url}
        </a>
      ) : null}

      {begun.user_code ? (
        <div className="flex items-center gap-2">
          <span className="text-micro text-ink-faint">Code to enter there</span>
          <code className="ident border border-rule px-1.5 py-0.5" data-testid="auth-user-code">
            {begun.user_code}
          </code>
        </div>
      ) : null}

      <Field
        label="Code from the browser"
        hint={
          windowClosed || expired
            ? "This window has closed. Start the sign-in again for a fresh code."
            : remaining
              ? `Paste the code it gives you back here. This window closes in ${remaining}.`
              : "Paste the code it gives you back here."
        }
      >
        <Input
          mono
          autoComplete="off"
          spellCheck={false}
          data-testid="input-auth-code"
          placeholder="paste the code"
          value={code}
          disabled={windowClosed || expired}
          onChange={(e) => setCode(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void submit();
          }}
        />
      </Field>

      {vaults.length ? (
        <Field
          label="Also save this login to a vault"
          hint="Every agent wired to that vault inherits it — convenient, but a paste-code login is a short-lived token, not a permanent one."
        >
          <Select
            data-testid="select-auth-vault"
            value={vault}
            onChange={(e) => setVault(e.target.value)}
          >
            <option value="">Just this agent</option>
            {vaults.map((v) => (
              <option key={v} value={v}>
                {v}
              </option>
            ))}
          </Select>
        </Field>
      ) : null}

      {error ? (
        <p className="text-micro" style={{ color: "var(--danger)" }} data-testid="auth-error">
          {error}
        </p>
      ) : null}

      {shareNote ? (
        <p className="text-micro text-ink-faint" data-testid="auth-vault-note">
          {shareNote}
        </p>
      ) : null}

      <div className="flex items-center gap-2">
        {windowClosed || expired ? (
          <Button size="sm" tone="primary" data-testid="btn-auth-restart" disabled={busy} onClick={begin}>
            {busy ? "Opening…" : "Start again"}
          </Button>
        ) : (
          <Button
            size="sm"
            tone="primary"
            data-testid="btn-auth-submit-code"
            disabled={!code.trim() || busy}
            onClick={submit}
          >
            {busy ? "Signing in…" : "Finish sign-in"}
          </Button>
        )}
        <Button
          size="sm"
          tone="ghost"
          data-testid="btn-auth-cancel"
          onClick={() => {
            setBegun(null);
            setExpiresAt(null);
            setError(null);
            setExpired(false);
          }}
        >
          Cancel
        </Button>
      </div>
    </div>
  );
}
