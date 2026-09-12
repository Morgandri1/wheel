"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * A Workflow Builder conversation for one project: the panel, the run behind it, and the one
 * question the panel cannot answer for itself — which credential the builder runs on.
 *
 * The builder spends the user's own Claude credential, so a project that has none cannot be
 * helped by retrying. The engine answers `needs_auth` with the agents and vaults that could be
 * designated instead, and this turns that into the choice a person can actually make.
 */
import { useCallback, useMemo, useState } from "react";
import { applyBoard as applyBoardCall } from "@/lib/api";
import { Button, Field, Input, Select } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";
import {
  BuilderStopped,
  builderCredential,
  builderTurns,
  type BuilderCredential,
  type BuilderMode,
} from "@/lib/builder-client";
import { isCredentialProblem, type BuilderSources } from "@/lib/builder-stream";
import type { ApplyOutcome } from "@/lib/board-apply";
import type { KnownNode } from "@/lib/workflow-proposal";
import { BuilderPanel, type BuilderTurn } from "@/components/builder/builder-panel";

export function BuilderSession({
  projectId,
  mode,
  known,
  onApplied,
}: {
  projectId: string;
  mode: BuilderMode;
  /** The board as it is now: the count for the plan, and the context an improve is read against. */
  known: KnownNode[];
  onApplied?: () => void;
}) {
  const [credential, setCredential] = useState<BuilderCredential>({ source: "builder" });
  const [needsAuth, setNeedsAuth] = useState<{ message: string; sources: BuilderSources } | null>(null);

  /**
   * The runner hands the panel text as it arrives. An `error` frame becomes a throw, so a run that
   * failed cannot be mistaken for an answer that merely said little.
   */
  const runner = useMemo(
    () =>
      async function* (turns: BuilderTurn[]) {
        const frames = builderTurns(projectId, { mode, turns, credential });
        for await (const frame of frames) {
          if (frame.kind === "delta") yield frame.text;
          else if (frame.kind === "error") throw new BuilderStopped({ status: 0, ...frame });
          else if (frame.boards > 1) {
            toast("The builder proposed more than one board; the last one is shown.");
          }
        }
      },
    [projectId, mode, credential],
  );

  const onRunnerError = useCallback((error: unknown) => {
    const refusal = error instanceof BuilderStopped ? error.refusal : null;
    if (refusal && isCredentialProblem(refusal.code)) {
      setNeedsAuth({
        message: refusal.message,
        sources: refusal.sources ?? { agents: [], vaults: [] },
      });
    }
  }, []);

  const applyBoard = useCallback(
    (board: unknown, dryRun: boolean, options: Parameters<typeof applyBoardCall>[3]) =>
      applyBoardCall(projectId, board, dryRun, options),
    [projectId],
  );

  return (
    <div className="flex h-full min-h-0 flex-col" data-testid="builder-session">
      {needsAuth ? (
        <CredentialPrompt
          projectId={projectId}
          message={needsAuth.message}
          sources={needsAuth.sources}
          onChosen={(chosen) => {
            setCredential(chosen);
            setNeedsAuth(null);
          }}
        />
      ) : null}
      <BuilderPanel
        runner={runner}
        applyBoard={applyBoard}
        currentNodes={known.length}
        known={known}
        onRunnerError={onRunnerError}
        onApplied={(outcome: ApplyOutcome) => {
          if (outcome.kind === "applied" || outcome.kind === "partial") onApplied?.();
        }}
      />
    </div>
  );
}

/**
 * Two ways out, both the user's own credential: point the builder at something this project
 * already has, or give it one of its own. A brand-new project has neither an agent nor a vault,
 * which is exactly why the second exists.
 */
function CredentialPrompt({
  projectId,
  message,
  sources,
  onChosen,
}: {
  projectId: string;
  message: string;
  sources: BuilderSources;
  onChosen: (credential: BuilderCredential) => void;
}) {
  const [key, setKey] = useState("");
  const [saving, setSaving] = useState(false);
  const options = [
    ...sources.agents.map((a) => ({ value: `agent:${a.id}`, label: `${a.name} (agent)` })),
    ...sources.vaults.map((v) => ({ value: `vault:${v.id}`, label: `${v.name} (vault)` })),
  ];

  const save = async () => {
    const value = key.trim();
    if (!value) return;
    setSaving(true);
    try {
      // `sk-ant-oat…` is a setup-token and must be filed as one; the engine refuses a value that
      // is not what the field claims, rather than storing it under a variable nothing reads.
      await builderCredential.put(
        projectId,
        value.startsWith("sk-ant-oat") ? { setup_token: value } : { api_key: value },
      );
      setKey("");
      toast("The builder can use this project's credential now.");
      onChosen({ source: "builder" });
    } catch (e) {
      toastError(e, "That credential was not accepted.");
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="border-l-2 border-[var(--wire-write)] px-3 py-2.5" data-testid="builder-needs-auth">
      <p className="text-meta text-ink">{message}</p>
      <p className="mt-1 text-micro text-ink-faint">
        The builder runs on your own Claude credential, inside this project — never a shared one.
      </p>

      {options.length ? (
        <Field label="Use a credential this project already has">
          <Select
            data-testid="select-builder-credential"
            defaultValue=""
            onChange={(e) => {
              const [source, node] = e.target.value.split(":");
              if (source === "agent" || source === "vault") onChosen({ source, node: node ?? "" });
            }}
          >
            <option value="" disabled>
              Choose an agent or vault…
            </option>
            {options.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </Select>
        </Field>
      ) : null}

      <Field
        label="Or give the builder its own"
        hint="An API key, or a token from `claude setup-token`. Stored in this project's engine."
      >
        <Input
          type="password"
          value={key}
          mono
          onChange={(e) => setKey(e.target.value)}
          placeholder="sk-ant-…"
          data-testid="input-builder-credential"
        />
      </Field>
      <div className="mt-2">
        <Button size="sm" onClick={save} disabled={!key.trim() || saving} data-testid="btn-builder-credential">
          {saving ? "Saving…" : "Save credential"}
        </Button>
      </div>
    </div>
  );
}
