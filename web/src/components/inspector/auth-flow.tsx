"use client";

import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Button, Field, Input, Select } from "@/components/ui";
import { OauthPanel } from "@/components/inspector/oauth-panel";
import { expiryMessage, vaultShareNote } from "@/lib/auth-session";
import { toast, toastError } from "@/components/ui/toast";
import type { EngineApi } from "@/lib/api";
import type { AuthStatus } from "@/lib/schema";

export type AuthView = "checking" | "stored" | "sign-in";

/**
 * Which of three states this panel is in. Pure, because the operator's bug was a decision rather
 * than a rendering: `?? false` collapsed "not read yet" into "no credential". See the tests.
 */
export function authView({
  credential,
  unreadable,
  agentRefusedCredentials,
  replacing,
}: {
  /** What `/auth` said, or null while it has not said anything yet. */
  credential: { authenticated: boolean } | null;
  /** `/auth` failed: a credential can be neither confirmed nor denied, and sign-in must stay reachable. */
  unreadable: boolean;
  /** The agent tried to start and refused what it had. Its verdict outranks the store. */
  agentRefusedCredentials: boolean;
  replacing: boolean;
}): AuthView {
  if (agentRefusedCredentials || replacing) return "sign-in";
  if (credential) return credential.authenticated ? "stored" : "sign-in";
  return unreadable ? "sign-in" : "checking";
}

/** What a stored credential is, in words. An unknown mode falls back rather than forcing a lockstep deploy. */
export function credentialLabel(
  mode: string | null | undefined,
  source: string | null | undefined,
  vaults: string[],
): string {
  if (mode === "env") {
    // An agent may be wired to several vaults, so the wire-list fallback can name the wrong one.
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

/** A stored credential and a refusing agent are not a contradiction to hide — an empty vault key is still a key. */
export function refusalCallout(hasStoredCredential: boolean, source: string | null | undefined): string {
  if (!hasStoredCredential) {
    return "This agent has no usable credentials, so it stopped at startup. Nothing sent to it was lost — anything queued stays queued and is delivered once a credential is saved.";
  }
  return `A credential is stored${source ? ` (from vault ${source})` : ""}, but this agent refused it at startup — stored is not the same as working. Signing in below replaces it. Nothing queued for the agent was lost.`;
}

export function AuthFlow({
  api,
  nodeId,
  needsAuth,
  vaults = [],
  onAuthenticated,
  nextNeedsAuth = null,
  onSelectAgent,
}: {
  api: EngineApi;
  nodeId: string;
  /** The agent tried to start and had no usable credentials — the one moment this panel is urgent. */
  needsAuth: boolean;
  /** Names of vaults this agent has a read wire to, for the `env` mode hint. */
  vaults?: string[];
  onAuthenticated: () => void;
  /** The next agent still waiting. One login per agent is the arrangement, so hand the next one over. */
  nextNeedsAuth?: { id: string; name: string } | null;
  onSelectAgent?: (id: string) => void;
}) {
  const agent = api.agent(nodeId);
  const [apiKey, setApiKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [justSaved, setJustSaved] = useState(false);
  const [savedHere, setSavedHere] = useState(false);
  const [replacing, setReplacing] = useState(false);
  const [setupToken, setSetupToken] = useState("");
  const [vault, setVault] = useState("");
  const [shareNote, setShareNote] = useState<string | null>(null);
  const [showOther, setShowOther] = useState(false);

  const status = useQuery({
    queryKey: ["auth", nodeId],
    queryFn: () => agent.authStatus(),
    // Poll only while a submission is settling. The engine writes credentials to disk and probes
    // the harness, so "authenticated" can land a beat after the request returns.
    refetchInterval: justSaved ? 2000 : false,
  });

  const credential = status.isSuccess ? status.data : null;
  const hasStoredCredential = credential?.authenticated === true;

  const view = authView({
    credential,
    unreadable: status.isError,
    // The agent judged the credential it HAD. Saving a new one in this panel retires that verdict.
    agentRefusedCredentials: needsAuth && !savedHere,
    replacing,
  });

  useEffect(() => {
    if (hasStoredCredential && justSaved) {
      setJustSaved(false);
      onAuthenticated();
    }
  }, [hasStoredCredential, justSaved, onAuthenticated]);

  const saveCredential = async (body: { api_key?: string; setup_token?: string }) => {
    setBusy(true);
    try {
      const next = (await agent.authComplete({
        ...body,
        ...(vault ? { save_to_vault: vault } : {}),
      })) as AuthStatus & {
        saved_to_vault?: { expires_at?: string | null; warning?: string | null } | null;
      };
      setApiKey("");
      setSetupToken("");
      setReplacing(false);
      setJustSaved(true);
      setSavedHere(true);
      setShareNote(vault ? vaultShareNote(next.saved_to_vault ?? {}) : null);
      if (next.authenticated) {
        toast("Credentials saved. Anything queued for this agent will be delivered.");
        onAuthenticated();
      }
      await status.refetch();
    } catch (e) {
      toastError(e, "The engine would not accept that credential.");
    } finally {
      setBusy(false);
    }
  };

  const save = async () => {
    const key = apiKey.trim();
    if (key) await saveCredential({ api_key: key });
  };

  const saveSetupToken = async () => {
    const token = setupToken.trim();
    if (token) await saveCredential({ setup_token: token });
  };

  const mode = credential?.mode;
  const source = credential?.source;
  const fromVault = mode === "env";

  // `null` means durable OR unknown, and the engine will not guess between them — so neither does this.
  const expiryNote = expiryMessage(credential?.expires_at, Date.now());

  const storedChip = (
    <div
      className="flex items-center justify-between gap-2 border border-rule px-2.5 py-2"
      data-testid="auth-status"
      data-authenticated={hasStoredCredential ? "true" : "false"}
      data-mode={mode ?? ""}
      data-source={source ?? ""}
    >
      <span className="text-micro text-ink-dim">
        {credentialLabel(mode, source, vaults)}
        {credential?.account ? ` · ${credential.account}` : ""}
      </span>
      {view !== "stored" ? null : fromVault ? (
        // This used to be a dead sentence ("edit it in the vault"), which left the browser sign-in
        // unreachable on the boards that need it most — a vault gets its FIRST value from here.
        <Button
          size="sm"
          tone="ghost"
          data-testid="btn-auth-different-account"
          onClick={() => {
            if (source) setVault(source);
            setReplacing(true);
          }}
        >
          Sign in with a different account…
        </Button>
      ) : (
        <Button size="sm" tone="ghost" data-testid="btn-auth-replace" onClick={() => setReplacing(true)}>
          Replace
        </Button>
      )}
    </div>
  );

  const nextAgentHop =
    nextNeedsAuth && onSelectAgent ? (
      <div className="flex items-center justify-between gap-2 border border-rule px-2.5 py-2">
        <span className="text-micro text-ink-dim">
          {nextNeedsAuth.name} is still waiting for credentials.
        </span>
        <Button
          size="sm"
          data-testid="btn-auth-next-agent"
          onClick={() => onSelectAgent(nextNeedsAuth.id)}
        >
          Sign in {nextNeedsAuth.name}
        </Button>
      </div>
    ) : null;

  if (view === "checking") {
    return (
      <div className="flex items-center border border-rule px-2.5 py-2" data-testid="auth-pending">
        <span className="flex h-7 items-center text-micro text-ink-faint">Checking credentials…</span>
      </div>
    );
  }

  if (view === "stored") {
    return (
      <div className="flex flex-col gap-2">
        {storedChip}
        {expiryNote ? (
          <p
            className="text-micro"
            data-testid="auth-expiry"
            style={{ color: expiryNote.urgent ? "var(--danger)" : "var(--ink-faint)" }}
          >
            {expiryNote.text}
          </p>
        ) : null}
        {shareNote ? (
          <p className="text-micro text-ink-faint" data-testid="auth-vault-note">
            {shareNote}
          </p>
        ) : null}
        {nextAgentHop}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-3 border border-rule p-2.5" data-testid="auth-flow">
      {hasStoredCredential ? storedChip : null}

      {needsAuth && !savedHere ? (
        <p data-testid="auth-needs-auth-callout" className="text-micro" style={{ color: "var(--danger)" }}>
          {refusalCallout(hasStoredCredential, source)}
        </p>
      ) : null}

      {status.isError ? (
        <p data-testid="auth-unreadable" className="text-micro text-ink-faint">
          This agent&rsquo;s credential status could not be read, so signing in is offered rather
          than assumed. Saving here is safe either way — it replaces whatever is stored.
        </p>
      ) : null}

      {/* The native path first: it needs no CLI, which is the whole reason it exists. */}
      <OauthPanel
        api={api}
        nodeId={nodeId}
        vaults={vaults}
        defaultVault={vault}
        onAuthenticated={() => {
          setJustSaved(true);
          setSavedHere(true);
          onAuthenticated();
          void status.refetch();
        }}
        onShareNote={setShareNote}
      />

      {/* While replacing, the section below is already open BECAUSE the person asked for it, so a
          toggle would be a control whose label disagrees with the screen. A toggle is only honest
          when it is the sole owner of the state it describes. */}
      {replacing ? null : (
        <div className="border-t border-rule pt-2.5">
          <button
            type="button"
            className="text-micro text-ink-faint underline"
            data-testid="btn-auth-other-ways"
            aria-expanded={showOther}
            aria-controls="auth-other-ways"
            onClick={() => setShowOther((v) => !v)}
          >
            {showOther ? "Hide other ways to sign in" : "Other ways to sign in"}
          </button>
        </div>
      )}

      {showOther || replacing ? (
        <div className="flex flex-col gap-3" id="auth-other-ways" data-testid="auth-other-ways">
          <Field
            label="Setup token"
            hint="From `claude setup-token` on a machine that has the CLI. Starts with sk-ant-oat."
          >
            <Input
              type="password"
              mono
              autoComplete="off"
              spellCheck={false}
              data-testid="input-setup-token"
              placeholder="sk-ant-oat…"
              value={setupToken}
              onChange={(e) => setSetupToken(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void saveSetupToken();
              }}
            />
          </Field>
          <Button
            size="sm"
            data-testid="btn-auth-save-setup-token"
            disabled={!setupToken.trim() || busy}
            onClick={saveSetupToken}
          >
            {busy ? "Saving…" : "Save setup token"}
          </Button>

          <Field
            label={replacing ? "New API key" : "API key"}
            hint={
              replacing
                ? "Replaces the credential already stored for this agent."
                : "An sk-ant- API key. Billed per token rather than against your plan — the account sign-in above is usually what you want."
            }
          >
            <Input
              type="password"
              mono
              autoComplete="off"
              spellCheck={false}
              data-testid="input-api-key"
              placeholder="sk-ant-…"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void save();
              }}
            />
          </Field>

          {vaults.length ? (
            <Field
              label="Also save to a vault"
              hint="Every agent wired to that vault inherits the credential."
            >
              <Select
                data-testid="select-auth-vault-other"
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
              <Button
                size="sm"
                tone="ghost"
                onClick={() => {
                  setReplacing(false);
                  setApiKey("");
                }}
              >
                Cancel
              </Button>
            ) : null}
            {justSaved && !hasStoredCredential ? (
              <span className="text-micro text-ink-faint" data-testid="auth-checking">
                Saving…
              </span>
            ) : null}
          </div>
        </div>
      ) : null}

      {shareNote ? (
        <p className="text-micro text-ink-faint" data-testid="auth-vault-note">
          {shareNote}
        </p>
      ) : null}
    </div>
  );
}
