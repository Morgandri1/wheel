// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * Board templates: `web/public/workflow_templates/*.json`, each one a validated board a user can
 * instantiate as their own project. `docs/proposals/wow-templates.md` §1: a template's `board`
 * field is byte-for-byte the same `EmittedBoard` shape `POST /v1/projects/instantiate` (and the
 * builder's `board/apply`) already accepts — no translation layer, no synthetic id, wires addressed
 * by node NAME because a template is static and names are already the stable reference.
 *
 * `crates/wheel-api/tests/template_gate.rs` is the build-time authority: every shipped file already
 * passed `apply::validate` plus per-node name/config checks before it could reach `public/`. This
 * parser still treats a file as untrusted at the boundary anyway — the same discipline
 * `parseProposal` uses for the builder's LLM output — because the gallery route reads whatever is on
 * disk at request time, and "already passed CI once" is not the same claim as "is this exact byte
 * stream well-formed right now."
 */
import { NODE_TYPES, type Capabilities, type NodeType, type Project, type WireType } from "@/lib/schema";
import { isWireAllowed } from "@/lib/wire-matrix";
import { validateNodeName } from "@/lib/validate";
import { capabilityWarnings } from "@/lib/workflow-proposal";
import type { Proposal } from "@/lib/workflow-proposal";
import type { ApplyReport, Refusal } from "@/lib/board-apply";

export interface TemplateNode {
  name: string;
  type: NodeType;
  config: Record<string, unknown>;
  position: { x: number; y: number };
}

export interface TemplateWire {
  from: string;
  to: string;
  type: WireType;
}

export interface TemplateBoard {
  nodes: TemplateNode[];
  wires: TemplateWire[];
}

export interface TemplateFile {
  title: string;
  description: string;
  requiresCapabilities: { http: boolean };
  board: TemplateBoard;
}

export type TemplateParseResult =
  | { status: "invalid"; problems: string[] }
  | { status: "ok"; template: TemplateFile; warnings: string[] };

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

/**
 * Parses and validates a template file's raw JSON. Mirrors `parseProposal`'s shape (untrusted
 * input, every problem collected rather than the first) but for the flat, name-addressed board a
 * template actually carries — see the module doc for why that shape differs from the builder's.
 */
export function parseTemplateFile(raw: unknown): TemplateParseResult {
  if (!isRecord(raw)) return { status: "invalid", problems: ["The template is not an object."] };

  const problems: string[] = [];
  const title = typeof raw.title === "string" ? raw.title.trim() : "";
  const description = typeof raw.description === "string" ? raw.description.trim() : "";
  if (!title) problems.push("The template has no `title`.");
  if (!description) problems.push("The template has no `description`.");

  const requiresCapabilities: { http: boolean } = {
    http: isRecord(raw.requires_capabilities) && raw.requires_capabilities.http === true,
  };

  if (!isRecord(raw.board) || !Array.isArray(raw.board.nodes)) {
    problems.push("The template's `board` has no `nodes` array.");
    return { status: "invalid", problems };
  }

  const names = new Set<string>();
  const nodes: TemplateNode[] = [];
  raw.board.nodes.forEach((rawNode: unknown, i: number) => {
    const where = `node ${i + 1}`;
    if (!isRecord(rawNode)) {
      problems.push(`${where} is not an object.`);
      return;
    }
    const name = typeof rawNode.name === "string" ? rawNode.name : "";
    const type = rawNode.type as NodeType;
    if (!NODE_TYPES.includes(type)) {
      problems.push(`${where} has an unknown type "${String(rawNode.type)}".`);
      return;
    }
    const nameProblem = validateNodeName(name, [...names], type);
    if (nameProblem) problems.push(`${where} ("${name}"): ${nameProblem}`);
    names.add(name);

    const pos = isRecord(rawNode.position) ? rawNode.position : {};
    nodes.push({
      name,
      type,
      config: isRecord(rawNode.config) ? rawNode.config : {},
      position: {
        x: typeof pos.x === "number" ? pos.x : 0,
        y: typeof pos.y === "number" ? pos.y : 0,
      },
    });
  });

  const byName = new Set(nodes.map((n) => n.name));
  const wires: TemplateWire[] = [];
  const rawWires = Array.isArray(raw.board.wires) ? raw.board.wires : [];
  rawWires.forEach((rawWire: unknown, i: number) => {
    if (!isRecord(rawWire)) {
      problems.push(`wire ${i + 1} is not an object.`);
      return;
    }
    const from = typeof rawWire.from === "string" ? rawWire.from : "";
    const to = typeof rawWire.to === "string" ? rawWire.to : "";
    const type = rawWire.type as WireType;
    const fromNode = nodes.find((n) => n.name === from);
    const toNode = nodes.find((n) => n.name === to);
    if (!byName.has(from) || !byName.has(to)) {
      problems.push(`wire ${i + 1} ("${from}" → "${to}") names a node that is not on the board.`);
      return;
    }
    if (!fromNode || !toNode || !isWireAllowed(fromNode.type, toNode.type, type)) {
      problems.push(
        `"${from}" (${fromNode?.type}) → "${to}" (${toNode?.type}) as \`${String(type)}\` is not an allowed wire.`,
      );
      return;
    }
    wires.push({ from, to, type });
  });

  const hasEndpoint = nodes.some((n) => n.type === "endpoint");
  if (hasEndpoint && !requiresCapabilities.http) {
    problems.push("The board has an endpoint node but does not declare `requires_capabilities.http`.");
  }

  if (problems.length) return { status: "invalid", problems };

  return {
    status: "ok",
    template: { title, description, requiresCapabilities, board: { nodes, wires } },
    warnings: capabilityWarnings(nodes),
  };
}

/**
 * A template's board, reshaped for `ProposalPreview` (`@/components/builder/builder-panel`) — the
 * same rendering the builder's own proposal uses, unchanged. A node's NAME stands in for the
 * synthetic id that rendering expects; names are already unique per board (enforced above), so this
 * is a lossless relabelling, not a guess.
 */
export function templateToProposal(board: TemplateBoard): Proposal {
  return {
    nodes: board.nodes.map((n) => ({
      id: n.name,
      name: n.name,
      type: n.type,
      position: n.position,
      config: n.config,
      wires: [],
    })),
    wires: board.wires.map((w) => ({ from: w.from, to: w.to, type: w.type })),
  };
}

/** What `POST /v1/projects/instantiate` sends, byte-for-byte (`docs/proposals/wow-templates-instantiate-route.md`). */
export interface InstantiateRequest {
  name: string;
  board: TemplateBoard;
  capabilities?: Capabilities;
}

export type InstantiateOutcome =
  | { kind: "created"; project: Project; report: ApplyReport }
  | { kind: "refused"; refusals: Refusal[]; message: string }
  | { kind: "rolled_back"; report: ApplyReport; projectId?: string; cleanedUp: boolean };

function isPlanWire(w: unknown): w is { from: string; to: string; type: WireType } {
  const c = w as { from?: unknown; to?: unknown; type?: unknown };
  return !!c && typeof c.from === "string" && typeof c.to === "string" && typeof c.type === "string";
}

function normaliseReport(r: unknown): ApplyReport {
  const c = isRecord(r) ? r : {};
  return {
    created_nodes: Array.isArray(c.created_nodes) ? c.created_nodes.filter((n) => typeof n === "string") : [],
    patched_nodes: Array.isArray(c.patched_nodes) ? c.patched_nodes.filter((n) => typeof n === "string") : [],
    created_wires: Array.isArray(c.created_wires) ? c.created_wires.filter(isPlanWire) : [],
    failures: Array.isArray(c.failures) ? (c.failures as ApplyReport["failures"]) : [],
  };
}

/**
 * Reads `POST /v1/projects/instantiate`'s response. `201` = full success, `422` = refused (nothing
 * created — identical shape to `board/apply`'s own 422, per the route doc), `207` = something failed
 * after a project was created and the server rolled it back (the overwhelmingly common case) or
 * could not (`rolled_back: false`, the one case with a real project id to show).
 *
 * Branches on the BODY's own fields, not the status code alone, for the same reason `readOutcome`
 * does (`@/lib/board-apply`): a status is corroboration, the shape is the fact.
 */
export function readInstantiateOutcome(status: number, body: unknown): InstantiateOutcome {
  const b = isRecord(body) ? body : {};

  if (status === 422 || Array.isArray(b.refusals)) {
    const refusals = Array.isArray(b.refusals)
      ? (b.refusals as { message?: unknown; refusal?: { code?: unknown } }[]).map((r) => ({
          code: typeof r.refusal?.code === "string" ? r.refusal.code : "refused",
          message: typeof r.message === "string" ? r.message : "No reason given.",
        }))
      : [];
    return {
      kind: "refused",
      refusals,
      message: typeof b.message === "string" ? b.message : "The board was refused.",
    };
  }

  // `applied: true` is the only claim of success this reads — a missing or non-true value is never
  // treated as created, matching `readOutcome`'s own rule that an absent boolean is not evidence.
  if (b.applied === true && isRecord(b.project)) {
    return { kind: "created", project: b.project as unknown as Project, report: normaliseReport(b.report) };
  }

  return {
    kind: "rolled_back",
    report: normaliseReport(b.report),
    projectId: typeof b.project_id === "string" ? b.project_id : undefined,
    cleanedUp: b.rolled_back !== false,
  };
}
