import type { NodeType, Position, WheelNode, WireType } from "@/lib/schema";
import type { Proposal } from "@/lib/workflow-proposal";

/** Only the two calls apply needs, so a test can drive it without a board or a network. */
export interface ApplyApi {
  createNode(input: { name: string; type: NodeType; position: Position; config?: unknown }): Promise<WheelNode>;
  createWire(from: string, to: string, type: WireType): Promise<unknown>;
}

export interface ApplyResult {
  createdNodes: { name: string; id: string }[];
  createdWires: { from: string; to: string; type: WireType }[];
  failures: { what: string; reason: string }[];
  /** True only when EVERYTHING landed. A partial apply is never reported as success. */
  complete: boolean;
}

const reason = (e: unknown) => (e instanceof Error ? e.message : String(e));

/**
 * Realise a validated proposal against a project: nodes first, then wires.
 *
 * Nodes before wires because a wire needs both endpoints to exist. The builder's ids are its own —
 * the engine assigns real ones — so wires are mapped through what was actually created, and a wire
 * whose endpoint failed to create is reported as a failure rather than sent with a dangling id.
 *
 * It does NOT stop at the first failure. A board that half-applies is a fact the user has to deal
 * with, and the useful thing is the complete list of what landed and what did not — stopping early
 * would hide the rest of the damage. `complete` is the single honest summary, and it is false if
 * anything at all failed (the proposal's success-shape invariant).
 */
export async function applyProposal(api: ApplyApi, proposal: Proposal): Promise<ApplyResult> {
  const idMap = new Map<string, string>();
  const result: ApplyResult = { createdNodes: [], createdWires: [], failures: [], complete: false };

  for (const node of proposal.nodes) {
    try {
      const created = await api.createNode({
        name: node.name,
        type: node.type,
        position: node.position,
        config: node.config,
      });
      idMap.set(node.id, created.id);
      result.createdNodes.push({ name: node.name, id: created.id });
    } catch (e) {
      result.failures.push({ what: `node "${node.name}"`, reason: reason(e) });
    }
  }

  for (const wire of proposal.wires) {
    const from = idMap.get(wire.from);
    const to = idMap.get(wire.to);
    const label = `wire ${nameOf(proposal, wire.from)} → ${nameOf(proposal, wire.to)} (${wire.type})`;
    if (!from || !to) {
      result.failures.push({ what: label, reason: "one of its nodes was not created" });
      continue;
    }
    try {
      await api.createWire(from, to, wire.type);
      result.createdWires.push({ from, to, type: wire.type });
    } catch (e) {
      // The engine enforces the wire matrix too. A refusal here is surfaced, never swallowed.
      result.failures.push({ what: label, reason: reason(e) });
    }
  }

  result.complete = result.failures.length === 0;
  return result;
}

function nameOf(proposal: Proposal, id: string): string {
  return proposal.nodes.find((n) => n.id === id)?.name ?? id;
}
