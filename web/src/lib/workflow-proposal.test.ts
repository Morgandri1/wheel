// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it } from "vitest";
import { END, START, extractBlock, parseProposal } from "./workflow-proposal";

const wrap = (obj: unknown) => `Here is the board.\n\n${START}\n${JSON.stringify(obj)}\n${END}\n`;

const agent = (id: string, name: string, wires: unknown[] = []) => ({
  id,
  name,
  type: "agent",
  position: { x: 0, y: 0 },
  config: { harness: "claude", system_prompt: "p" },
  wires,
});
const ctx = (id: string, name: string, wires: unknown[] = []) => ({
  id,
  name,
  type: "ctx",
  position: { x: 0, y: 0 },
  config: { markdown: "" },
  wires,
});

describe("extractBlock — a conversational turn is not an error", () => {
  it("reports none for ordinary prose", () => {
    expect(extractBlock("What should this workflow do?")).toEqual({ status: "none" });
  });

  it("reports unterminated while a block is still streaming, rather than invalid", () => {
    // Mid-stream the closing delimiter has not arrived. Calling that malformed would flash an
    // error at the user for the normal case of a reply still being written.
    expect(extractBlock(`${START}\n{"nodes":[`)).toEqual({ status: "unterminated" });
  });

  it("takes the LAST block, so an iterated conversation applies the newest proposal", () => {
    const text = `${START}\n{"v":1}\n${END}\nrevised:\n${START}\n{"v":2}\n${END}`;
    const b = extractBlock(text);
    expect(b.status).toBe("ok");
    if (b.status === "ok") expect(JSON.parse(b.json)).toEqual({ v: 2 });
  });
});

describe("parseProposal — the builder's output is untrusted", () => {
  it("accepts a legal board and flattens its wires for the apply step", () => {
    const r = parseProposal(wrap({ nodes: [ctx("i1", "brief", [{ to: "i2", type: "send" }]), agent("i2", "worker")] }));
    expect(r.status).toBe("ok");
    if (r.status !== "ok") return;
    expect(r.proposal.nodes.map((n) => n.name)).toEqual(["brief", "worker"]);
    expect(r.proposal.wires).toEqual([{ from: "i1", to: "i2", type: "send" }]);
  });

  it("REFUSES a wire the matrix denies, naming both ends", () => {
    // The engine would refuse this too. Catching it here is what stops a half-applied board.
    const r = parseProposal(wrap({ nodes: [agent("i1", "a"), agent("i2", "b", [{ to: "i1", type: "read" }])] }));
    expect(r.status).toBe("invalid");
    if (r.status !== "invalid") return;
    expect(r.problems[0] ?? "").toMatch(/"b" \(agent\) → "a" \(agent\) as `read` is not an allowed wire/);
  });

  it("refuses a wire pointing at an id that is not in the workflow", () => {
    const r = parseProposal(wrap({ nodes: [agent("i1", "a", [{ to: "nope", type: "send" }])] }));
    expect(r.status).toBe("invalid");
    if (r.status === "invalid") expect(r.problems[0] ?? "").toMatch(/not in this workflow/);
  });

  it("refuses duplicate ids and duplicate names rather than silently merging nodes", () => {
    const dupId = parseProposal(wrap({ nodes: [agent("same", "a"), agent("same", "b")] }));
    expect(dupId.status).toBe("invalid");
    const dupName = parseProposal(wrap({ nodes: [agent("i1", "a"), agent("i2", "a")] }));
    expect(dupName.status).toBe("invalid");
  });

  it("refuses an unknown node type and an illegal name instead of trusting the model", () => {
    const badType = parseProposal(wrap({ nodes: [{ ...agent("i1", "a"), type: "sudo" }] }));
    expect(badType.status).toBe("invalid");
    if (badType.status === "invalid") expect(badType.problems[0] ?? "").toMatch(/unknown type "sudo"/);

    const badName = parseProposal(wrap({ nodes: [agent("i1", "Not A Name")] }));
    expect(badName.status).toBe("invalid");
  });

  it("never throws on garbage, it reports", () => {
    expect(parseProposal(`${START}\nnot json at all\n${END}`).status).toBe("invalid");
    expect(parseProposal(`${START}\n{"nodes":"nope"}\n${END}`).status).toBe("invalid");
    expect(parseProposal(`${START}\n[]\n${END}`).status).toBe("invalid");
    expect(parseProposal(`${START}\nnull\n${END}`).status).toBe("invalid");
  });

  /**
   * Read out of the engine rather than believed: a codex node is REFUSED at creation
   * (`reject_unsupported_harness`), so this board does not apply at all — it does not apply and
   * sit idle. Script, chest and mcp nodes are creatable but inert, which is a different warning.
   */
  it("warns about what the board can hold but cannot run, without refusing it", () => {
    const r = parseProposal(
      wrap({ nodes: [{ ...agent("i1", "coder"), config: { harness: "codex", system_prompt: "p" } }] }),
    );
    expect(r.status).toBe("ok");
    if (r.status === "ok") expect(r.warnings[0] ?? "").toMatch(/codex.*engine refuses/i);
  });

  it("says which of the inert node types will not do anything yet", () => {
    const inert = (id: string, name: string, type: string, config: Record<string, unknown>) => ({
      id,
      name,
      type,
      position: { x: 0, y: 0 },
      config,
      wires: [],
    });
    const r = parseProposal(
      wrap({
        nodes: [
          inert("i1", "runner", "script", { language: "python", source: "x" }),
          inert("i2", "files", "chest", {}),
          inert("i3", "server", "mcp", { transport: "stdio", command: "c" }),
        ],
      }),
    );
    expect(r.status).toBe("ok");
    if (r.status !== "ok") return;
    const warnings = r.warnings.join(" ");
    expect(warnings).toMatch(/nothing executes scripts yet/i);
    expect(warnings).toMatch(/storage is not implemented/i);
    expect(warnings).toMatch(/does not attach its tools yet/i);
  });

  it("defaults a missing position rather than rejecting the board over it", () => {
    const r = parseProposal(wrap({ nodes: [{ ...agent("i1", "a"), position: undefined }] }));
    expect(r.status).toBe("ok");
    if (r.status === "ok") expect(r.proposal.nodes[0]?.position).toEqual({ x: 0, y: 0 });
  });
});
