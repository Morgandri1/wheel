// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import type { AgentNode } from "@/lib/schema";

/**
 * `wheel_core::BudgetStatus`, from SDK's `GET /v1/board` follow-up to wow-agent-brief.md #6
 * (PR #56, not yet merged/regenerated into `schema/generated.ts` as of this file). Hand-typed
 * here rather than in the generated file on purpose: `pnpm gen:types` will add the real field to
 * `NodeState` once #56 lands, and `readBudgetStatus` below is the ONE place that needs updating
 * then — delete the cast, keep the accessor. Everything downstream (the component, its tests)
 * reads through that accessor rather than the raw field, so nothing else has to change.
 */
export interface BudgetStatus {
  max_turns?: number;
  pct_of_max_turns?: number;
  max_usd?: number;
  pct_of_max_usd?: number;
}

/**
 * `undefined` (no field), `null` (present but empty) and a real object all mean the same thing
 * here — no ceiling to show — so this collapses them rather than making three call sites decide.
 */
export function readBudgetStatus(node: AgentNode): BudgetStatus | null {
  const state = node.state as { budget_status?: BudgetStatus | null } | null | undefined;
  return state?.budget_status ?? null;
}

export interface BudgetLine {
  /** "turns" or "usd" — which ceiling this line is about. */
  kind: "turns" | "usd";
  /** e.g. "3 / 10 turns" or "$4.20 / $10.00". */
  label: string;
  pct: number;
  /** Past 90% is close enough to `budget_exhausted` to call out, not just report. */
  near: boolean;
}

const fmtUsd = (n: number) => `$${n.toFixed(2)}`;

/**
 * One line per ceiling actually configured — a `BudgetStatus` with neither `max_turns` nor
 * `max_usd` should not have existed (SDK's `compute` returns `None` for that case), but this
 * still degrades to an empty list rather than a line about nothing, if it ever does.
 */
export function budgetLines(status: BudgetStatus, spendTurns: number, spendUsd: number): BudgetLine[] {
  const lines: BudgetLine[] = [];
  if (status.max_turns !== undefined) {
    const pct = status.pct_of_max_turns ?? 0;
    lines.push({
      kind: "turns",
      label: `${spendTurns} / ${status.max_turns} turns`,
      pct,
      near: pct >= 90,
    });
  }
  if (status.max_usd !== undefined) {
    const pct = status.pct_of_max_usd ?? 0;
    lines.push({
      kind: "usd",
      label: `${fmtUsd(spendUsd)} / ${fmtUsd(status.max_usd)}`,
      pct,
      near: pct >= 90,
    });
  }
  return lines;
}
