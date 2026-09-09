// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it } from "vitest";
import {
  parseTemplateFile,
  readInstantiateOutcome,
  templateToProposal,
  type TemplateBoard,
} from "./templates";

const agentNode = (name: string) => ({
  name,
  type: "agent",
  config: { harness: "claude", system_prompt: "p" },
  position: { x: 0, y: 0 },
});
const ctxNode = (name: string) => ({ name, type: "ctx", config: { markdown: "x" } });
const vaultNode = (name: string, keys: string[] = []) => ({
  name,
  type: "vault",
  config: { keys },
});

const file = (overrides: Record<string, unknown> = {}) => ({
  title: "Research crew",
  description: "An agent and its notes.",
  board: { nodes: [ctxNode("notes"), agentNode("researcher")], wires: [{ from: "notes", to: "researcher", type: "send" }] },
  ...overrides,
});

describe("parseTemplateFile — a shipped template is still treated as untrusted input", () => {
  it("accepts a legal board, addressed by name with no synthetic id", () => {
    const r = parseTemplateFile(file());
    expect(r.status).toBe("ok");
    if (r.status !== "ok") return;
    expect(r.template.title).toBe("Research crew");
    expect(r.template.board.nodes.map((n) => n.name)).toEqual(["notes", "researcher"]);
    expect(r.template.board.wires).toEqual([{ from: "notes", to: "researcher", type: "send" }]);
    expect(r.template.requiresCapabilities).toEqual({ http: false });
  });

  it("carries requires_capabilities.http through verbatim", () => {
    const r = parseTemplateFile(file({ requires_capabilities: { http: true } }));
    expect(r.status).toBe("ok");
    if (r.status === "ok") expect(r.template.requiresCapabilities).toEqual({ http: true });
  });

  it("rejects a missing title or description", () => {
    const r1 = parseTemplateFile(file({ title: "" }));
    expect(r1.status).toBe("invalid");
    if (r1.status === "invalid") expect(r1.problems.join()).toMatch(/no `title`/);

    const r2 = parseTemplateFile(file({ description: "   " }));
    expect(r2.status).toBe("invalid");
    if (r2.status === "invalid") expect(r2.problems.join()).toMatch(/no `description`/);
  });

  it("rejects a node with an invalid name", () => {
    const r = parseTemplateFile(file({ board: { nodes: [ctxNode("Bad Name")], wires: [] } }));
    expect(r.status).toBe("invalid");
    if (r.status === "invalid") expect(r.problems.join()).toMatch(/lowercase/);
  });

  it("applies the table-specific name rule to table nodes", () => {
    const r = parseTemplateFile(
      file({ board: { nodes: [{ name: "bad-table", type: "table", config: { columns: [] } }], wires: [] } }),
    );
    expect(r.status).toBe("invalid");
    if (r.status === "invalid") expect(r.problems.join()).toMatch(/sqlite table/);
  });

  it("rejects a wire to a node that is not on the board", () => {
    const r = parseTemplateFile(
      file({ board: { nodes: [agentNode("a")], wires: [{ from: "a", to: "ghost", type: "send" }] } }),
    );
    expect(r.status).toBe("invalid");
    if (r.status === "invalid") expect(r.problems.join()).toMatch(/not on the board/);
  });

  it("rejects a wire the matrix forbids", () => {
    const r = parseTemplateFile(
      file({
        board: {
          nodes: [agentNode("a"), vaultNode("v")],
          wires: [{ from: "v", to: "a", type: "read" }],
        },
      }),
    );
    expect(r.status).toBe("invalid");
    if (r.status === "invalid") expect(r.problems.join()).toMatch(/not an allowed wire/);
  });

  it("rejects an endpoint node without a matching requires_capabilities.http", () => {
    const r = parseTemplateFile(
      file({
        board: {
          nodes: [{ name: "hook", type: "endpoint", config: { method: "POST", path: "/hook", response_mode: "ack" } }],
          wires: [],
        },
      }),
    );
    expect(r.status).toBe("invalid");
    if (r.status === "invalid") expect(r.problems.join()).toMatch(/requires_capabilities\.http/);
  });

  it("accepts the same endpoint once the capability is declared", () => {
    const r = parseTemplateFile(
      file({
        requires_capabilities: { http: true },
        board: {
          nodes: [{ name: "hook", type: "endpoint", config: { method: "POST", path: "/hook", response_mode: "ack" } }],
          wires: [],
        },
      }),
    );
    expect(r.status).toBe("ok");
  });

  it("warns, but does not refuse, on a codex agent or a script node", () => {
    const r = parseTemplateFile(
      file({
        board: {
          nodes: [
            { name: "codex-agent", type: "agent", config: { harness: "codex", system_prompt: "p" } },
            { name: "runner", type: "script", config: { language: "python", source: "print(1)" } },
          ],
          wires: [],
        },
      }),
    );
    expect(r.status).toBe("ok");
    if (r.status === "ok") {
      expect(r.warnings.some((w) => w.includes("codex"))).toBe(true);
      expect(r.warnings.some((w) => w.includes("script"))).toBe(true);
    }
  });

  it("does not crash on a non-object payload", () => {
    expect(parseTemplateFile(null).status).toBe("invalid");
    expect(parseTemplateFile("just a string").status).toBe("invalid");
    expect(parseTemplateFile([1, 2, 3]).status).toBe("invalid");
  });

  it("does not crash when a node is not an object or a wire is not an object", () => {
    const r = parseTemplateFile(file({ board: { nodes: ["not an object"], wires: ["also not"] } }));
    expect(r.status).toBe("invalid");
    if (r.status === "invalid") {
      expect(r.problems.some((p) => p.includes("is not an object"))).toBe(true);
    }
  });
});

describe("templateToProposal — reuses the builder's own preview rendering", () => {
  it("relabels each node's name as its id, losslessly", () => {
    const board: TemplateBoard = {
      nodes: [
        { name: "notes", type: "ctx", config: { markdown: "x" }, position: { x: 1, y: 2 } },
        { name: "researcher", type: "agent", config: {}, position: { x: 3, y: 4 } },
      ],
      wires: [{ from: "notes", to: "researcher", type: "send" }],
    };
    const proposal = templateToProposal(board);
    expect(proposal.nodes).toEqual([
      { id: "notes", name: "notes", type: "ctx", position: { x: 1, y: 2 }, config: { markdown: "x" }, wires: [] },
      { id: "researcher", name: "researcher", type: "agent", position: { x: 3, y: 4 }, config: {}, wires: [] },
    ]);
    expect(proposal.wires).toEqual([{ from: "notes", to: "researcher", type: "send" }]);
  });
});

describe("readInstantiateOutcome — the instantiate route's response", () => {
  it("reads 201 as created, carrying the new project", () => {
    const o = readInstantiateOutcome(201, {
      applied: true,
      project: { id: "p1", owner_id: "u1", name: "Research crew", capabilities: { http: false }, status: "starting" },
      report: { created_nodes: ["a"], patched_nodes: [], created_wires: [], failures: [] },
    });
    expect(o.kind).toBe("created");
    if (o.kind === "created") expect(o.project.id).toBe("p1");
  });

  it("never reads created from a status alone — applied must be true and project must be present", () => {
    expect(readInstantiateOutcome(201, { applied: true }).kind).not.toBe("created");
    expect(readInstantiateOutcome(201, { project: { id: "p1" } }).kind).not.toBe("created");
    expect(readInstantiateOutcome(201, {}).kind).not.toBe("created");
  });

  it("reads 422 as refused, with the same shape board/apply already uses", () => {
    const o = readInstantiateOutcome(422, {
      applied: false,
      message: "the board was refused; nothing was created",
      refusals: [{ refusal: { code: "wire_not_allowed" }, message: "no read wire is allowed" }],
    });
    expect(o.kind).toBe("refused");
    if (o.kind === "refused") {
      expect(o.message).toMatch(/nothing was created/);
      expect(o.refusals).toEqual([{ code: "wire_not_allowed", message: "no read wire is allowed" }]);
    }
  });

  it("reads a rolled-back 207 as rolled_back with cleanedUp true", () => {
    const o = readInstantiateOutcome(207, {
      applied: false,
      rolled_back: true,
      report: { created_nodes: ["a"], patched_nodes: [], created_wires: [], failures: [{ step: "capabilities", error: "x" }] },
    });
    expect(o.kind).toBe("rolled_back");
    if (o.kind === "rolled_back") {
      expect(o.cleanedUp).toBe(true);
      expect(o.projectId).toBeUndefined();
      expect(o.report.failures).toHaveLength(1);
    }
  });

  it("reads a failed rollback as rolled_back with cleanedUp false and a project id to show", () => {
    const o = readInstantiateOutcome(207, {
      applied: false,
      rolled_back: false,
      project_id: "p2",
      report: { created_nodes: [], patched_nodes: [], created_wires: [], failures: [] },
    });
    expect(o.kind).toBe("rolled_back");
    if (o.kind === "rolled_back") {
      expect(o.cleanedUp).toBe(false);
      expect(o.projectId).toBe("p2");
    }
  });

  it("degrades an unreadable body to rolled_back rather than throwing or claiming success", () => {
    expect(readInstantiateOutcome(500, null).kind).toBe("rolled_back");
    expect(readInstantiateOutcome(500, "not an object").kind).toBe("rolled_back");
  });
});
