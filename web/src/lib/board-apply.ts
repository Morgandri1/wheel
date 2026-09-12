// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import type { NodeType, WireType } from "@/lib/schema";

export interface PlanWire {
  from: string;
  to: string;
  type: WireType;
}

export interface PatchDetail {
  name: string;
  /** The fields this patch writes. Everything else keeps whatever it holds now. */
  fields: string[];
  /** Fields where a WHOLE list is replaced — RFC 7386 has no element-wise array merge. */
  replaced_arrays: string[];
}

export interface DeleteNode {
  name: string;
  type: NodeType;
  /** Wires that go when the node does. A consequence, not a second decision. */
  wires: PlanWire[];
}

export interface ApplyPlan {
  create_nodes: string[];
  patch_nodes: string[];
  create_wires: PlanWire[];
  patch_details: PatchDetail[];
  delete_wires: PlanWire[];
  delete_nodes: DeleteNode[];
}

/** What the user is being asked to allow, ready-made by the server rather than derived here. */
export interface ApplyConsent {
  would_modify?: string[];
  would_wire?: PlanWire[];
  would_delete?: { name: string; type: NodeType; destroys: string }[];
  would_unwire?: PlanWire[];
  /** Set these and re-apply. */
  grant: ApplyGrantName[];
}

export type ApplyGrantName = "allow_patch" | "allow_wire" | "allow_delete" | "allow_unwire";

/** The four consents, as the apply call carries them. */
export type ApplyGrants = Partial<Record<ApplyGrantName, boolean>>;

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
  deleted_nodes: string[];
  deleted_wires: PlanWire[];
  failures: ApplyFailure[];
}

export interface Refusal {
  code: string;
  message: string;
}

export type ApplyOutcome =
  | { kind: "plan"; plan: ApplyPlan; digest: string | null }
  | { kind: "applied"; report: ApplyReport }
  | { kind: "partial"; report: ApplyReport }
  | { kind: "refused"; refusals: Refusal[]; message: string; consent: ApplyConsent | null }
  /**
   * The board moved between the plan the user read and the apply they pressed, or a destructive
   * apply arrived without confirming a plan at all. Its own outcome because the answer is neither
   * "it failed" nor "try again": the user has to read the NEW plan before any of it happens.
   */
  | { kind: "stale"; plan: ApplyPlan; digest: string | null; message: string };

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
    plan_digest?: unknown;
    code?: unknown;
    report?: ApplyReport;
    refusals?: { message?: string; refusal?: { code?: string } }[];
    consent?: ApplyConsent;
    message?: string;
  };
  const digest = typeof b.plan_digest === "string" ? b.plan_digest : null;

  if (status === 422 || Array.isArray(b.refusals)) {
    return {
      kind: "refused",
      message: typeof b.message === "string" ? b.message : "The board was refused.",
      refusals: (b.refusals ?? []).map((r) => ({
        code: r.refusal?.code ?? "refused",
        message: r.message ?? "No reason given.",
      })),
      consent: normaliseConsent(b.consent),
    };
  }

  // The plan the user read is gone: either it changed underneath, or a destructive apply arrived
  // without confirming one. Never treated as a failure to retry — the new plan has to be read.
  if (status === 409 && b.plan) {
    return {
      kind: "stale",
      plan: normalisePlan(b.plan),
      digest,
      message:
        typeof b.message === "string"
          ? b.message
          : "The board changed since this plan was shown. Nothing was applied.",
    };
  }

  if (b.plan) return { kind: "plan", plan: normalisePlan(b.plan), digest };

  const report = normaliseReport(b.report);
  // `applied` is the authority. A missing field is treated as NOT applied: claiming success from an
  // absent boolean is exactly the direction this must never fail in.
  return b.applied === true ? { kind: "applied", report } : { kind: "partial", report };
}

const normalisePlan = (p: ApplyPlan): ApplyPlan => ({
  create_nodes: p.create_nodes ?? [],
  patch_nodes: p.patch_nodes ?? [],
  create_wires: (p.create_wires ?? []).filter(isWire),
  patch_details: p.patch_details ?? [],
  delete_wires: (p.delete_wires ?? []).filter(isWire),
  delete_nodes: p.delete_nodes ?? [],
});

const normaliseReport = (r: ApplyReport | undefined): ApplyReport => ({
  created_nodes: r?.created_nodes ?? [],
  patched_nodes: r?.patched_nodes ?? [],
  created_wires: (r?.created_wires ?? []).filter(isWire),
  deleted_nodes: r?.deleted_nodes ?? [],
  deleted_wires: (r?.deleted_wires ?? []).filter(isWire),
  failures: r?.failures ?? [],
});

/**
 * A consent prompt is only worth showing if it names something AND says which flag answers it. A
 * half-read one would render a button that grants nothing.
 */
function normaliseConsent(c: ApplyConsent | undefined): ApplyConsent | null {
  if (!c || !Array.isArray(c.grant) || c.grant.length === 0) return null;
  return {
    would_modify: c.would_modify ?? [],
    would_wire: (c.would_wire ?? []).filter(isWire),
    would_delete: c.would_delete ?? [],
    would_unwire: (c.would_unwire ?? []).filter(isWire),
    grant: c.grant,
  };
}

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
  // Removals are named last and named plainly. "Change 3 nodes" reads as editing; this does not.
  if (plan.delete_wires.length) parts.push(`remove ${plan.delete_wires.length} wire${s(plan.delete_wires.length)}`);
  if (plan.delete_nodes.length) parts.push(`DELETE ${plan.delete_nodes.length} node${s(plan.delete_nodes.length)}`);
  return parts.length ? parts.join(", ") : "change nothing — the board already matches";
}

const s = (n: number) => (n === 1 ? "" : "s");

/** The board's own limit: past this the canvas is the bottleneck, not the API. */
export const BOARD_NODE_SOFT_CAP = 200;

/**
 * What the board will hold AFTER this plan, not just what the plan adds.
 *
 * The API's 200-node cap bounds ONE REQUEST, not the project (API, 2026-09-07; §3e's per-project
 * cap is documented but unimplemented). So a board grows without limit an apply at a time, and a
 * user confirming "create 40 nodes" has no way to see they are going from 180 to 220. The delta is
 * what they are approving; the total is what they have to live with.
 */
export function resultingNodeCount(currentNodes: number, plan: ApplyPlan): number {
  return currentNodes + plan.create_nodes.length;
}

export function boardSizeWarning(currentNodes: number, plan: ApplyPlan): string | null {
  const after = resultingNodeCount(currentNodes, plan);
  if (after <= BOARD_NODE_SOFT_CAP) return null;
  return `This takes the board to ${after} nodes. Past ${BOARD_NODE_SOFT_CAP} the canvas gets slow, and nothing on the server stops it growing further.`;
}

/** Does this plan destroy anything? Apply confirms the plan it was shown when so. */
export function planDestroys(plan: ApplyPlan): boolean {
  return plan.delete_nodes.length > 0 || plan.delete_wires.length > 0;
}

/**
 * What each deletion costs, said per node type.
 *
 * One sentence for every type is a sentence people skim. A vault and a ctx node do not lose the
 * same thing, and the whole reason this is in front of the user is that they can still say no.
 */
export function deletionCost(type: NodeType): string {
  switch (type) {
    case "table":
      return "its rows are dropped and cannot be recovered";
    case "vault":
      return "the secrets it holds are destroyed";
    case "chest":
      return "the files it holds are destroyed";
    case "agent":
      return "its messages, logs and stored credential go with it";
    case "ctx":
      return "the markdown it injects is lost";
    case "script":
      return "its source is lost";
    case "endpoint":
      return "its public URL stops answering";
    default:
      return "the agents wired to it lose that capability";
  }
}

/** What a patch writes, for a confirm step that says which fields rather than "it changes". */
export function patchSummary(detail: PatchDetail): string {
  const fields = detail.fields.join(", ") || "nothing";
  if (detail.replaced_arrays.length === 0) return fields;
  return `${fields} (${detail.replaced_arrays.join(", ")} replaced as a whole list)`;
}
