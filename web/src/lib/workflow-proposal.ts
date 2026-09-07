import { NODE_TYPES, type NodeType, type WireType } from "@/lib/schema";
import { isWireAllowed } from "@/lib/wire-matrix";
import { validateNodeName } from "@/lib/validate";

export const START = "---START-WORKFLOW---";
export const END = "---END-WORKFLOW---";

export interface ProposedNode {
  id: string;
  name: string;
  type: NodeType;
  position: { x: number; y: number };
  config: Record<string, unknown>;
  wires: { to: string; type: WireType }[];
}

export interface Proposal {
  nodes: ProposedNode[];
  /** Flattened for rendering and for the apply step, which creates nodes before wires. */
  wires: { from: string; to: string; type: WireType }[];
}

export type ProposalResult =
  | { status: "none" }
  | { status: "unterminated" }
  | { status: "invalid"; problems: string[] }
  | { status: "ok"; proposal: Proposal; warnings: string[] };

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

/**
 * Everything here treats the builder's output as UNTRUSTED. It is LLM-emitted JSON that will become
 * real nodes and wires, so the UI validates it before it is ever shown as applicable, and never
 * throws on bad input — a crash tells the user nothing about what the builder got wrong.
 *
 * The delimiters are the prompt's own contract. A turn with no block is an ordinary conversational
 * turn, which is the common case and not an error.
 */
export function extractBlock(text: string): { status: "none" | "unterminated" } | { status: "ok"; json: string } {
  const start = text.lastIndexOf(START);
  if (start === -1) return { status: "none" };
  const after = start + START.length;
  const end = text.indexOf(END, after);
  // A block still streaming in is not a malformed block; the caller waits rather than showing an error.
  if (end === -1) return { status: "unterminated" };
  return { status: "ok", json: text.slice(after, end).trim() };
}

export function parseProposal(text: string): ProposalResult {
  const block = extractBlock(text);
  if (block.status !== "ok") return { status: block.status };

  let parsed: unknown;
  try {
    parsed = JSON.parse(block.json);
  } catch (e) {
    return { status: "invalid", problems: [`The workflow block is not valid JSON: ${(e as Error).message}`] };
  }
  if (!isRecord(parsed) || !Array.isArray(parsed.nodes)) {
    return { status: "invalid", problems: ["The workflow has no `nodes` array."] };
  }

  const problems: string[] = [];
  const warnings: string[] = [];
  const nodes: ProposedNode[] = [];
  const byId = new Map<string, ProposedNode>();
  const names = new Set<string>();

  parsed.nodes.forEach((raw: unknown, i: number) => {
    const where = `node ${i + 1}`;
    if (!isRecord(raw)) {
      problems.push(`${where} is not an object.`);
      return;
    }
    const id = typeof raw.id === "string" ? raw.id : "";
    const name = typeof raw.name === "string" ? raw.name : "";
    const type = raw.type as NodeType;

    if (!id) problems.push(`${where} has no id.`);
    else if (byId.has(id)) problems.push(`${where} reuses the id of "${byId.get(id)?.name}".`);
    if (!NODE_TYPES.includes(type)) {
      problems.push(`${where} has an unknown type "${String(raw.type)}".`);
      return;
    }
    const nameProblem = validateNodeName(name, [...names]);
    if (nameProblem) problems.push(`${where} ("${name}"): ${nameProblem}`);
    names.add(name);

    const pos = isRecord(raw.position) ? raw.position : {};
    const node: ProposedNode = {
      id,
      name,
      type,
      position: {
        x: typeof pos.x === "number" ? pos.x : 0,
        y: typeof pos.y === "number" ? pos.y : 0,
      },
      config: isRecord(raw.config) ? raw.config : {},
      wires: [],
    };
    if (id) byId.set(id, node);
    nodes.push(node);
  });

  const wires: Proposal["wires"] = [];
  parsed.nodes.forEach((raw: unknown, i: number) => {
    if (!isRecord(raw) || !Array.isArray(raw.wires)) return;
    const from = byId.get(typeof raw.id === "string" ? raw.id : "");
    if (!from) return;
    raw.wires.forEach((w: unknown) => {
      if (!isRecord(w)) {
        problems.push(`node ${i + 1} has a wire that is not an object.`);
        return;
      }
      const to = byId.get(typeof w.to === "string" ? w.to : "");
      const type = w.type as WireType;
      if (!to) {
        problems.push(`"${from.name}" has a wire to an id that is not in this workflow.`);
        return;
      }
      // The same default-DENY matrix the engine enforces. Catching it here means the user sees the
      // refusal in the preview instead of a half-applied board.
      if (!isWireAllowed(from.type, to.type, type)) {
        problems.push(
          `"${from.name}" (${from.type}) → "${to.name}" (${to.type}) as \`${String(type)}\` is not an allowed wire.`,
        );
        return;
      }
      from.wires.push({ to: to.id, type });
      wires.push({ from: from.id, to: to.id, type });
    });
  });

  if (problems.length) return { status: "invalid", problems };

  // Capability warnings, not refusals: the board can hold these, they just will not run yet.
  for (const n of nodes) {
    if (n.type === "agent" && n.config.harness === "codex") {
      warnings.push(`"${n.name}" uses the codex harness, which is not runnable yet.`);
    }
    if (n.type === "script") {
      warnings.push(`"${n.name}" is a script node; script execution is not live yet.`);
    }
  }

  return { status: "ok", proposal: { nodes, wires }, warnings };
}
