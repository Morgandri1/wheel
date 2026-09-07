import type { WireType } from "@/lib/schema";

export interface PlanWire {
  from: string;
  to: string;
  type: WireType;
}

export interface ApplyPlan {
  create_nodes: string[];
  patch_nodes: string[];
  create_wires: PlanWire[];
}

export interface ApplyFailure {
  step: string;
  error: string;
  /** Present when the step was about a wire — lets the failure be drawn ON the edge. */
  wire?: PlanWire;
  /** Present when the step was about a node. */
  node?: string;
}

export interface ApplyReport {
  created_nodes: string[];
  patched_nodes: string[];
  created_wires: PlanWire[];
  failures: ApplyFailure[];
}

export interface Refusal {
  code: string;
  message: string;
}

export type ApplyOutcome =
  | { kind: "plan"; plan: ApplyPlan }
  | { kind: "applied"; report: ApplyReport }
  | { kind: "partial"; report: ApplyReport }
  | { kind: "refused"; refusals: Refusal[]; message: string };

/**
 * Branch on `applied`, never on the status code.
 *
 * 207 is a 2xx and means the board is PARTLY realised. A client that treats 2xx as fine reports a
 * half-applied board as success — the single outcome this whole step exists to prevent — so the
 * boolean is the authority and the status is only corroboration (API's board-apply-shape.md).
 */
export function readOutcome(status: number, body: unknown): ApplyOutcome {
  const b = (body ?? {}) as {
    applied?: unknown;
    plan?: ApplyPlan;
    report?: ApplyReport;
    refusals?: { message?: string; refusal?: { code?: string } }[];
    message?: string;
  };

  if (status === 422 || Array.isArray(b.refusals)) {
    return {
      kind: "refused",
      message: typeof b.message === "string" ? b.message : "The board was refused.",
      refusals: (b.refusals ?? []).map((r) => ({
        code: r.refusal?.code ?? "refused",
        message: r.message ?? "No reason given.",
      })),
    };
  }

  if (b.plan) return { kind: "plan", plan: normalisePlan(b.plan) };

  const report = normaliseReport(b.report);
  // `applied` is the authority. A missing field is treated as NOT applied: claiming success from an
  // absent boolean is exactly the direction this must never fail in.
  return b.applied === true ? { kind: "applied", report } : { kind: "partial", report };
}

const normalisePlan = (p: ApplyPlan): ApplyPlan => ({
  create_nodes: p.create_nodes ?? [],
  patch_nodes: p.patch_nodes ?? [],
  create_wires: (p.create_wires ?? []).filter(isWire),
});

const normaliseReport = (r: ApplyReport | undefined): ApplyReport => ({
  created_nodes: r?.created_nodes ?? [],
  patched_nodes: r?.patched_nodes ?? [],
  created_wires: (r?.created_wires ?? []).filter(isWire),
  failures: r?.failures ?? [],
});

/**
 * Kept deliberately: API pinned the structured shape with tests, but a wire that does not match it
 * is dropped from the styled render rather than guessed at. A shape change should degrade to
 * "not drawn", never to "drawn as the wrong wire type".
 */
function isWire(w: unknown): w is PlanWire {
  const c = w as PlanWire;
  return !!c && typeof c.from === "string" && typeof c.to === "string" && typeof c.type === "string";
}

/** What the plan will change, in one sentence, for the confirm button's own label. */
export function planSummary(plan: ApplyPlan): string {
  const parts: string[] = [];
  if (plan.create_nodes.length) parts.push(`create ${plan.create_nodes.length} node${s(plan.create_nodes.length)}`);
  if (plan.patch_nodes.length) parts.push(`change ${plan.patch_nodes.length} node${s(plan.patch_nodes.length)}`);
  if (plan.create_wires.length) parts.push(`add ${plan.create_wires.length} wire${s(plan.create_wires.length)}`);
  return parts.length ? parts.join(", ") : "change nothing — the board already matches";
}

const s = (n: number) => (n === 1 ? "" : "s");
