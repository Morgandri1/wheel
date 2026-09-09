"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { useState } from "react";
import { Button, Field, Textarea } from "@/components/ui";
import { NODE_META, WIRE_META } from "@/lib/node-meta";
import { boardSizeWarning, planSummary, resultingNodeCount, type ApplyOutcome } from "@/lib/board-apply";
import { START, extractBlock, parseProposal, type Proposal } from "@/lib/workflow-proposal";

export interface BuilderTurn {
  role: "user" | "builder";
  text: string;
}

/**
 * Sends the conversation and streams the builder's reply. SDK owns the run; this is the seam it
 * plugs into, so the panel is buildable and testable before any backend exists.
 */
export type BuilderRunner = (turns: BuilderTurn[]) => AsyncIterable<string>;

/** `dry_run` returns the plan to confirm; a second call with dryRun=false performs it. */
export type BoardApplier = (board: unknown, dryRun: boolean) => Promise<ApplyOutcome>;

/**
 * The proposed board is shown BEFORE it is applied, and applying is a separate, deliberate click.
 *
 * The builder is an LLM writing a board that becomes real nodes, wires and agents. Auto-applying
 * would let a model's output take effect with nobody having read it, so the preview is not a
 * nicety — it is the review step.
 */
export function BuilderPanel({
  runner,
  applyBoard,
  currentNodes = 0,
  onApplied,
}: {
  runner: BuilderRunner | null;
  applyBoard: BoardApplier;
  /** Nodes already on the board, so the plan can show the TOTAL and not only the delta. */
  currentNodes?: number;
  onApplied?: (outcome: ApplyOutcome) => void;
}) {
  const [turns, setTurns] = useState<BuilderTurn[]>([]);
  const [draft, setDraft] = useState("");
  const [streaming, setStreaming] = useState("");
  const [busy, setBusy] = useState(false);
  const [applying, setApplying] = useState(false);
  const [outcome, setOutcome] = useState<ApplyOutcome | null>(null);
  const [planning, setPlanning] = useState(false);

  const last = [...turns].reverse().find((t) => t.role === "builder");
  const parsed = parseProposal(last?.text ?? "");
  const proposal = parsed.status === "ok" ? parsed.proposal : null;
  // The board as the builder emitted it, sent verbatim: the server validates it again and its plan
  // is what the user confirms. The client parse above is a fast local refusal, not the authority.
  const rawBoard = proposal ? rawBlock(last?.text ?? "") : null;

  const send = async () => {
    const text = draft.trim();
    if (!text || !runner || busy) return;
    const next: BuilderTurn[] = [...turns, { role: "user", text }];
    setTurns(next);
    setDraft("");
    setBusy(true);
    setOutcome(null);
    let acc = "";
    try {
      for await (const chunk of runner(next)) {
        acc += chunk;
        setStreaming(acc);
      }
      setTurns([...next, { role: "builder", text: acc }]);
    } catch (e) {
      setTurns([...next, { role: "builder", text: `The builder stopped: ${(e as Error).message}` }]);
    } finally {
      setStreaming("");
      setBusy(false);
    }
  };

  // Two calls, deliberately: dry_run asks the SERVER what it would do, and only then does the user
  // confirm. The client's own validation is a fast refusal, not the authority — the plan the user
  // approves is the server's, so what they confirm is what will happen.
  const preview = async () => {
    if (!rawBoard) return;
    setPlanning(true);
    try {
      setOutcome(await applyBoard(rawBoard, true));
    } finally {
      setPlanning(false);
    }
  };

  const apply = async () => {
    if (!rawBoard) return;
    setApplying(true);
    try {
      const o = await applyBoard(rawBoard, false);
      setOutcome(o);
      onApplied?.(o);
    } finally {
      setApplying(false);
    }
  };

  return (
    <div className="flex h-full flex-col gap-3 p-4" data-testid="builder-panel">
      <div className="flex-1 space-y-3 overflow-y-auto">
        {turns.length === 0 && !streaming ? (
          <p className="text-meta text-ink-dim" data-testid="builder-empty">
            Describe what you want this workflow to do. The builder proposes a board; nothing is
            created until you apply it.
          </p>
        ) : null}
        {turns.map((t, i) => (
          <p
            key={i}
            className={t.role === "user" ? "text-meta text-ink" : "text-meta text-ink-dim"}
            data-testid={t.role === "user" ? "builder-turn-user" : "builder-turn-builder"}
          >
            {visibleText(t.text)}
          </p>
        ))}
        {streaming ? (
          <p className="text-meta text-ink-dim" data-testid="builder-streaming">
            {visibleText(streaming)}
          </p>
        ) : null}
      </div>

      {parsed.status === "invalid" ? (
        <div className="border-l-2 border-[var(--danger)] px-2.5 py-2" data-testid="builder-invalid">
          <p className="text-micro text-ink-dim">
            The builder proposed a board that cannot be applied as written:
          </p>
          <ul className="mt-1 space-y-0.5">
            {parsed.problems.map((p) => (
              <li key={p} className="text-micro text-ink-faint">
                {p}
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {proposal ? (
        <ProposalPreview proposal={proposal} warnings={parsed.status === "ok" ? parsed.warnings : []} />
      ) : null}

      {outcome ? <Outcome outcome={outcome} currentNodes={currentNodes} /> : null}

      <Field label="Message the builder">
        <Textarea
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          rows={3}
          placeholder={
            runner ? "A researcher that reads a brief and reports daily…" : "The builder is not connected yet."
          }
          disabled={!runner || busy}
          data-testid="builder-input"
        />
      </Field>

      <div className="flex items-center gap-2">
        <Button size="sm" onClick={send} disabled={!runner || busy || !draft.trim()} data-testid="btn-builder-send">
          {busy ? "Thinking…" : "Send"}
        </Button>
        <Button
          size="sm"
          onClick={preview}
          disabled={!rawBoard || planning}
          data-testid="btn-builder-preview"
          title={rawBoard ? undefined : "The builder has not proposed a board yet."}
        >
          {planning ? "Checking…" : "Check what this would do"}
        </Button>
        <Button
          size="sm"
          onClick={apply}
          disabled={!rawBoard || applying || outcome?.kind !== "plan"}
          data-testid="btn-builder-apply"
          title={outcome?.kind === "plan" ? undefined : "Check the plan first — apply confirms it."}
        >
          {applying ? "Applying…" : "Apply"}
        </Button>
        {!runner ? (
          <span className="text-micro text-ink-faint" data-testid="builder-unavailable">
            The builder run is not wired up yet, so this panel cannot talk to it.
          </span>
        ) : null}
      </div>
    </div>
  );
}

function rawBlock(text: string): unknown {
  const b = extractBlock(text);
  if (b.status !== "ok") return null;
  try {
    return JSON.parse(b.json);
  } catch {
    return null;
  }
}

/** The delimited JSON is machinery, not conversation: the preview renders it, the transcript hides it. */
function visibleText(text: string): string {
  const start = text.indexOf(START);
  return (start === -1 ? text : text.slice(0, start)).trim() || "(proposed a board)";
}

/** Exported so the template gallery can render a preview identical to the builder's own. */
export function ProposalPreview({ proposal, warnings }: { proposal: Proposal; warnings: string[] }) {
  return (
    <div className="border border-rule p-2.5" data-testid="builder-preview">
      <p className="mb-1.5 text-micro text-ink-faint">
        Proposed: {proposal.nodes.length} nodes, {proposal.wires.length} wires. Nothing exists until
        you apply it.
      </p>
      <ul className="space-y-0.5">
        {proposal.nodes.map((n) => (
          <li key={n.id} className="text-micro text-ink-dim" data-testid="builder-preview-node">
            <span style={{ color: NODE_META[n.type].tint }}>{NODE_META[n.type].label}</span>{" "}
            <span className="ident text-ink">{n.name}</span>
          </li>
        ))}
        {proposal.wires.map((w, i) => {
          const from = proposal.nodes.find((n) => n.id === w.from)?.name ?? w.from;
          const to = proposal.nodes.find((n) => n.id === w.to)?.name ?? w.to;
          return (
            <li key={`${w.from}-${w.to}-${i}`} className="text-micro text-ink-faint" data-testid="builder-preview-wire">
              {from} → {to} <span style={{ color: WIRE_META[w.type].color }}>{w.type}</span>
            </li>
          );
        })}
      </ul>
      {warnings.map((w) => (
        <p key={w} className="mt-1 text-micro text-[var(--danger)]" data-testid="builder-warning">
          {w}
        </p>
      ))}
    </div>
  );
}

/**
 * All four outcomes get their own shape, because they need four different reactions:
 *   plan     — nothing has happened yet; this is what Apply will do
 *   applied  — everything landed
 *   partial  — SOME of it landed, and the board is now half-built. Not an error to swallow: the
 *              user has to know which half, and re-applying is how they finish it.
 *   refused  — nothing was created, so the board is untouched and safe to re-emit
 */
function Outcome({ outcome, currentNodes }: { outcome: ApplyOutcome; currentNodes: number }) {
  if (outcome.kind === "plan") {
    return (
      <div className="border-l-2 border-[var(--wire-read)] px-2.5 py-2" data-testid="builder-plan">
        <p className="text-micro text-ink">This will {planSummary(outcome.plan)}.</p>
        {outcome.plan.patch_nodes.length ? (
          <p className="text-micro text-ink-faint">
            Changes existing nodes: {outcome.plan.patch_nodes.join(", ")}
          </p>
        ) : null}
        <p className="text-micro text-ink-faint" data-testid="builder-plan-total">
          Board goes from {currentNodes} to {resultingNodeCount(currentNodes, outcome.plan)} nodes.
        </p>
        {boardSizeWarning(currentNodes, outcome.plan) ? (
          <p className="text-micro text-[var(--danger)]" data-testid="builder-plan-size-warning">
            {boardSizeWarning(currentNodes, outcome.plan)}
          </p>
        ) : null}
        <p className="text-micro text-ink-faint">Nothing has been created yet.</p>
      </div>
    );
  }

  if (outcome.kind === "refused") {
    return (
      <div className="border-l-2 border-[var(--danger)] px-2.5 py-2" data-testid="builder-refused">
        <p className="text-micro text-ink">{outcome.message}</p>
        {outcome.refusals.map((r) => (
          <p key={`${r.code}-${r.message}`} className="text-micro text-ink-faint">
            {r.message}
          </p>
        ))}
        <p className="text-micro text-ink-faint">
          Your board is untouched — ask the builder to fix these and try again.
        </p>
      </div>
    );
  }

  const { report } = outcome;
  const partial = outcome.kind === "partial";
  return (
    <div
      className={`border-l-2 px-2.5 py-2 ${partial ? "border-[var(--danger)]" : "border-[var(--live)]"}`}
      data-testid={partial ? "builder-partial" : "builder-applied"}
    >
      <p className="text-micro text-ink">
        {partial
          ? `Partly applied. ${report.created_nodes.length} nodes and ${report.created_wires.length} wires landed; ${report.failures.length} did not.`
          : `Applied: ${report.created_nodes.length} nodes, ${report.created_wires.length} wires.`}
      </p>
      {report.failures.map((f) => (
        <p key={f.step} className="text-micro text-ink-faint" data-testid="builder-failure">
          {f.wire ? `${f.wire.from} → ${f.wire.to} (${f.wire.type})` : (f.node ?? f.step)}: {f.error}
        </p>
      ))}
      {partial ? (
        <p className="text-micro text-ink-faint">
          The board is half-built. Applying again creates only what is still missing.
        </p>
      ) : null}
    </div>
  );
}
