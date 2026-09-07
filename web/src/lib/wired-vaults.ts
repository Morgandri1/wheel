import type { WheelNode } from "@/lib/schema";

export type VaultNode = Extract<WheelNode, { type: "vault" }>;

/**
 * The vault nodes `from` may read — the only ones any picker is allowed to offer.
 *
 * One rule, one place. It was written twice already (tool fills §3d, endpoint bearer §3) and a
 * third caller is coming for agent workspaces. Three copies of a rule is where it drifts, and the
 * copy that lags is the one that silently offers a vault the node cannot read — a picker that
 * builds a config the engine will refuse, or worse, one the operator believes is wired.
 *
 * `from` is passed in rather than inferred. The wire direction is per node type — tool→vault and
 * endpoint→vault are read wires from those nodes, and the agent picker keys on the agent's own —
 * so a component that guesses whose wires to consult breaks on the caller after next.
 */
export function wiredVaults(from: WheelNode, nodes: WheelNode[]): VaultNode[] {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  return (from.wires ?? [])
    .filter((w) => w.type === "read")
    .map((w) => byId.get(w.to))
    .filter((n): n is VaultNode => n?.type === "vault");
}
