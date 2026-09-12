// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

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
  /** What the builder asks to take away. Never inferred from what it left out. */
  remove: { nodes: string[]; wires: { from: string; to: string; type: WireType }[] };
}

/** A node already on the board, so an improve proposal can be read against it. */
export interface KnownNode {
  id: string;
  name: string;
  type: NodeType;
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

/**
 * @param known Nodes already on the board. An improve proposal emits only what it CHANGES, and
 * wires at existing nodes by id, so without this every such wire reads as pointing at nothing.
 */
export function parseProposal(text: string, known: KnownNode[] = []): ProposalResult {
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
  const nodes: ProposedNode[] = [];
  const byId = new Map<string, ProposedNode>();
  const names = new Set<string>();
  // Existing nodes are addressable but not re-declared: a wire may name one, and a name that is
  // already taken is a CHANGE to that node rather than a duplicate.
  const existingById = new Map(known.map((n) => [n.id, n]));
  const existingByName = new Map(known.map((n) => [n.name, n]));
  const typeOf = (ref: ProposedNode | KnownNode) => ref.type;

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
    // A name that belongs to an existing node is that node; only a collision WITHIN the proposal
    // is a duplicate.
    const taken = existingByName.has(name) ? [...names] : [...names, ...existingByName.keys()];
    const nameProblem = validateNodeName(name, taken.filter((n) => n !== name).concat([...names]));
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
      const id = typeof w.to === "string" ? w.to : "";
      const to = byId.get(id) ?? existingById.get(id);
      const type = w.type as WireType;
      if (!to) {
        problems.push(`"${from.name}" has a wire to an id that is not in this workflow.`);
        return;
      }
      // The same default-DENY matrix the engine enforces. Catching it here means the user sees the
      // refusal in the preview instead of a half-applied board.
      if (!isWireAllowed(from.type, typeOf(to), type)) {
        problems.push(
          `"${from.name}" (${from.type}) → "${to.name}" (${typeOf(to)}) as \`${String(type)}\` is not an allowed wire.`,
        );
        return;
      }
      from.wires.push({ to: to.id, type });
      wires.push({ from: from.id, to: to.id, type });
    });
  });

  const remove = readRemovals(parsed.remove, known, problems);

  if (problems.length) return { status: "invalid", problems };

  return {
    status: "ok",
    proposal: { nodes, wires, remove },
    warnings: capabilityWarnings(nodes).concat(removalWarnings(remove, known)),
  };
}

/**
 * The `remove` block, which is the ONLY way a proposal takes something away — a node the builder
 * simply stopped mentioning is left alone, here and at the server.
 */
function readRemovals(
  raw: unknown,
  known: KnownNode[],
  problems: string[],
): Proposal["remove"] {
  const empty = { nodes: [] as string[], wires: [] as Proposal["wires"] };
  if (raw === undefined) return empty;
  if (!isRecord(raw)) {
    problems.push("`remove` is not an object.");
    return empty;
  }
  const names = new Set(known.map((n) => n.name));
  const nodes = (Array.isArray(raw.nodes) ? raw.nodes : []).flatMap((n: unknown) => {
    if (typeof n !== "string") {
      problems.push("`remove.nodes` must name nodes.");
      return [];
    }
    if (!names.has(n)) {
      problems.push(`The builder asks to remove "${n}", which is not on this board.`);
      return [];
    }
    return [n];
  });
  const wires = (Array.isArray(raw.wires) ? raw.wires : []).flatMap((w: unknown) => {
    if (!isRecord(w) || typeof w.from !== "string" || typeof w.to !== "string") {
      problems.push("`remove.wires` must name wires as {from, to, type}.");
      return [];
    }
    return [{ from: w.from, to: w.to, type: w.type as WireType }];
  });
  return { nodes, wires };
}

/** Removals are shown as warnings in the preview too: the plan is the gate, this is the heads-up. */
function removalWarnings(remove: Proposal["remove"], known: KnownNode[]): string[] {
  const typeOf = new Map(known.map((n) => [n.name, n.type]));
  return remove.nodes.map((name) => {
    const type = typeOf.get(name);
    return `"${name}"${type ? ` (${type})` : ""} would be REMOVED, and what it holds is destroyed.`;
  });
}

/**
 * Capability warnings, not refusals: the board can hold these, they just will not run yet.
 *
 * Shared between the builder's proposal preview and the template gallery — both show a person a
 * `{nodes, wires}` shape before anything is created, so both owe the same honesty about what will
 * not actually run. Keeping this in one place means a new capability restriction only has to be
 * taught here once.
 */
export function capabilityWarnings(
  nodes: { name: string; type: NodeType; config: Record<string, unknown> }[],
): string[] {
  const warnings: string[] = [];
  for (const n of nodes) {
    // Verified against the engine, not assumed: a codex node is REFUSED at creation
    // (`reject_unsupported_harness`), so this board will not apply at all rather than apply and
    // sit idle. The others are creatable but inert.
    if (n.type === "agent" && n.config.harness === "codex") {
      warnings.push(`"${n.name}" uses the codex harness, which the engine refuses — this board will not apply.`);
    }
    if (n.type === "script") {
      warnings.push(`"${n.name}" is a script node; nothing executes scripts yet, so it will sit there.`);
    }
    if (n.type === "chest") {
      warnings.push(`"${n.name}" is a chest; its storage is not implemented yet, so nothing can read or write it.`);
    }
    if (n.type === "mcp") {
      warnings.push(`"${n.name}" is an mcp node; wiring it to an agent does not attach its tools yet.`);
    }
  }
  return warnings;
}
