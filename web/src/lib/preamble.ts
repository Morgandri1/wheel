/**
 * Reconstructs, for the inspector, exactly what an agent is told about itself —
 * the §3 "## WHEEL board — agent orchestration" block, its `wheel connections`
 * wire list, and the ctx markdown injected on every start and context clear.
 *
 * This is a read-only preview. The engine composes the real thing; if the two
 * ever drift, the engine is right and this file is the bug.
 */
import type { AgentNode, CtxNode, NodeType, WheelNode, WireType } from "@/lib/schema";
import { wireRule } from "@/lib/wire-matrix";

export interface WireLine {
  direction: "out" | "in";
  /** The other end. */
  name: string;
  nodeType: NodeType;
  type: WireType;
  /** Plain language, from this agent's point of view. */
  semantics: string;
  injection: boolean;
}

/** Every wire touching `agent`, outgoing first, each in the agent's own voice. */
export function agentWireLines(nodes: readonly WheelNode[], agent: AgentNode): WireLine[] {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const out: WireLine[] = [];

  for (const wire of (agent.wires ?? [])) {
    const target = byId.get(wire.to);
    if (!target) continue;
    const rule = wireRule(agent.type, target.type, wire.type);
    out.push({
      direction: "out",
      name: target.name,
      nodeType: target.type,
      type: wire.type,
      semantics: rule?.outgoing ?? "you can access it",
      injection: rule?.injection ?? false,
    });
  }

  const incoming: WireLine[] = [];
  for (const node of nodes) {
    if (node.id === agent.id) continue;
    for (const wire of (node.wires ?? [])) {
      if (wire.to !== agent.id) continue;
      const rule = wireRule(node.type, agent.type, wire.type);
      incoming.push({
        direction: "in",
        name: node.name,
        nodeType: node.type,
        type: wire.type,
        semantics: rule?.incoming ?? "it can reach you",
        injection: rule?.injection ?? false,
      });
    }
  }

  return [...out, ...incoming];
}

/** The ctx nodes whose markdown is prepended to this agent's prompt. */
export function injectedContexts(nodes: readonly WheelNode[], agent: AgentNode): CtxNode[] {
  return nodes.filter(
    (n): n is CtxNode =>
      n.type === "ctx" && (n.wires ?? []).some((w) => w.to === agent.id && w.type === "send"),
  );
}

const pad = (s: string, width: number) => s + " ".repeat(Math.max(0, width - s.length));

/** The `Your wires:` block, column-aligned the way the CLI prints it. */
export function renderWireList(lines: readonly WireLine[], indent = "            "): string {
  if (lines.length === 0) return "Your wires: (none yet — this agent is on its own)";
  const nameWidth = Math.max(...lines.map((l) => l.name.length));
  const typeWidth = Math.max(...lines.map((l) => l.type.length));
  return lines
    .map((line, i) => {
      const arrow = line.direction === "out" ? "→" : "←";
      const body = `${arrow} ${pad(line.name, nameWidth)}  ${pad(line.type, typeWidth)}  ${line.semantics}`;
      return i === 0 ? `Your wires: ${body}` : `${indent}${body}`;
    })
    .join("\n");
}

/** The generated orchestration block, verbatim per §3. */
export function renderOrchestrationBlock(
  nodes: readonly WheelNode[],
  agent: AgentNode,
  projectName: string,
): string {
  return [
    "## WHEEL board — agent orchestration",
    `You are "${agent.name}", an agent on a Wheel board (project ${projectName}).`,
    'To message a connected agent, run:  wheel msg "TARGET" "your message"',
    "Your identity is proven from your own credentials — you never pass it.",
    "## Board memory (durable, wire-gated)",
    '  wheel read <node> · wheel write <node> "<value>" · wheel read/write <table>/<row> · wheel ls <table> · wheel secret get <vault>/<key> · wheel run <script>',
    "You can only read/write nodes you're wired to — run `wheel connections` to see yours.",
    renderWireList(agentWireLines(nodes, agent)),
  ].join("\n");
}

/** Everything the child process receives as its system prompt, in order. */
export function renderFullPreamble(
  nodes: readonly WheelNode[],
  agent: AgentNode,
  projectName: string,
): string {
  const parts: string[] = [];
  if (agent.config.system_prompt.trim()) parts.push(agent.config.system_prompt.trim());
  parts.push(renderOrchestrationBlock(nodes, agent, projectName));
  for (const ctx of injectedContexts(nodes, agent)) {
    parts.push(`# Context: ${ctx.name}\n${ctx.config.markdown}`);
  }
  return parts.join("\n\n");
}

/** The §3 envelope a message arrives in. Used by the drawer to show real framing. */
export function renderAgentPrompt(
  from: { name: string; type: NodeType | "user" },
  body: string,
  id = "01ARZ3NDEKTSV4RRFFQ69G5FAV",
): string {
  const escaped = body.replaceAll("</AgentPrompt>", "<\\/AgentPrompt>");
  return `<AgentPrompt id="${id}" from="${from.name}" type="${from.type}">\n${escaped}\n</AgentPrompt>`;
}
