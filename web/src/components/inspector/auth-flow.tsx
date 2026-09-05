"use client";

import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Button, CopyField, Field, Input } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";
import type { AuthBeginResponse } from "@/lib/schema";
import type { EngineApi } from "@/lib/api";

/**
 * Signing an agent in. The harness decides the shape — a device code, a code to paste back,
 * or an API key — so this renders whatever `auth/begin` says and polls until it takes.
 */
export function AuthFlow({
  api,
  nodeId,
  onAuthenticated,
}: {
  api: EngineApi;
  nodeId: string;
  onAuthenticated: () => void;
}) {
  const agent = api.agent(nodeId);
  const [begun, setBegun] = useState<AuthBeginResponse | null>(null);
  const [code, setCode] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [busy, setBusy] = useState(false);

  const status = useQuery({
    queryKey: ["auth", nodeId],
    queryFn: () => agent.authStatus(),
    refetchInterval: begun && !busy ? 2500 : false,
  });

  useEffect(() => {
    if (status.data?.authenticated) {
      setBegun(null);
      onAuthenticated();
    }
  }, [status.data?.authenticated, onAuthenticated]);

  if (status.data?.authenticated) {
    return (
      <div className="flex items-center justify-between gap-2 border border-rule px-2.5 py-2">
        <span className="text-micro text-ink-dim">
          Signed in{status.data.account ? ` as ${status.data.account}` : ""}
        </span>
        <span className="h-1.5 w-1.5 rounded-full" style={{ background: "var(--live)" }} />
      </div>
    );
  }

  if (!begun) {
    return (
      <Button
        data-testid="btn-authenticate"
        onClick={async () => {
          try {
            setBusy(true);
            setBegun(await agent.authBegin());
          } catch (e) {
            toastError(e, "Couldn't start sign-in.");
          } finally {
            setBusy(false);
          }
        }}
      >
        {busy ? "Starting…" : "Authenticate"}
      </Button>
    );
  }

  const complete = async (body: { code?: string; api_key?: string }) => {
    try {
      setBusy(true);
      const next = await agent.authComplete(body);
      if (next.authenticated) {
        toast("Agent signed in.");
        setBegun(null);
        onAuthenticated();
      }
    } catch (e) {
      toastError(e, "That didn't complete sign-in.");
    } finally {
      setBusy(false);
      void status.refetch();
    }
  };

  return (
    <div className="flex flex-col gap-3 border border-rule p-2.5" data-testid="auth-flow">
      <p className="text-micro text-ink-dim">{begun.instructions}</p>

      {begun.user_code ? (
        <Field label="Your code">
          <CopyField value={begun.user_code} testId="auth-user-code" />
        </Field>
      ) : null}

      {begun.url ? (
        <a
          href={begun.url}
          target="_blank"
          rel="noreferrer"
          data-testid="auth-link"
          className="text-micro underline underline-offset-4"
          style={{ color: "var(--wire-read)" }}
        >
          Open {new URL(begun.url).host}
        </a>
      ) : null}

      {begun.mode === "api_key" ? (
        <Field label="API key" hint="Stored in the container, never shown again.">
          <Input
            data-testid="input-api-key"
            type="password"
            mono
            value={apiKey}
            onChange={(e) => setApiKey(e.target.value)}
            placeholder="sk-…"
          />
        </Field>
      ) : begun.mode === "paste_code" ? (
        <Field label="Code from the browser">
          <Input data-testid="input-auth-code" mono value={code} onChange={(e) => setCode(e.target.value)} />
        </Field>
      ) : null}

      <div className="flex items-center gap-2">
        <Button
          tone="primary"
          size="sm"
          data-testid="btn-auth-complete"
          disabled={busy || (begun.mode === "api_key" && !apiKey)}
          onClick={() => complete(begun.mode === "api_key" ? { api_key: apiKey } : { code: code || begun.user_code })}
        >
          {busy ? "Checking…" : begun.mode === "api_key" ? "Save key" : "I've done it"}
        </Button>
        <Button size="sm" tone="ghost" onClick={() => setBegun(null)}>
          Cancel
        </Button>
      </div>
    </div>
  );
}
