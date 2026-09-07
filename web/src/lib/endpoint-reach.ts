import type { EndpointNode, WheelNode } from "@/lib/schema";

/**
 * Who on this board an anonymous internet client can reach through this endpoint.
 *
 * Neither half of this is a defect. `auth: none` is the deliberate design — an endpoint has to be
 * usable as a webhook with any provider — and an agent with no public input is unremarkable. The
 * composition is what matters: wire an unauthenticated endpoint into an agent and anyone holding
 * the URL is writing into a bypassPermissions agent's inbox. Nothing in the product said so
 * (ADVERSARY 043), so the panel has to.
 *
 * Scripts count. A `send` wire runs one with the request, which is code execution on the same
 * unauthenticated path; leaving them out would repeat the omission this exists to fix.
 */
export function publicReach(
  endpoint: EndpointNode,
  nodes: WheelNode[],
): { name: string; type: "agent" | "script" }[] {
  // Absent auth is `none`: the field is optional and the engine's default is unauthenticated, so
  // treating "not set" as safe would be exactly the silent composition this warns about.
  if (endpoint.config.auth && endpoint.config.auth.mode !== "none") return [];

  const byId = new Map(nodes.map((n) => [n.id, n]));
  const reached: { name: string; type: "agent" | "script" }[] = [];
  // `wires` is optional in the schema; absent means nothing consumes this endpoint yet.
  for (const wire of endpoint.wires ?? []) {
    if (wire.type !== "send") continue;
    const target = byId.get(wire.to);
    if (target?.type === "agent" || target?.type === "script") {
      reached.push({ name: target.name, type: target.type });
    }
  }
  return reached;
}

/** What the operator has built, in the terms they can act on — never the word "insecure". */
export function reachSentence(reached: { name: string; type: "agent" | "script" }[]): string | null {
  if (reached.length === 0) return null;
  const names = reached.map((r) => r.name);
  const list =
    names.length === 1
      ? names[0]
      : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
  const verb = reached.every((r) => r.type === "script") ? "run" : "send messages to";
  return `This endpoint has no authentication, so anyone who knows its URL can ${verb} ${list}.`;
}
