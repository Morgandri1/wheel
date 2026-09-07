import { describe, expect, it, vi } from "vitest";
import { applyProposal, type ApplyApi } from "./workflow-apply";
import type { Proposal } from "./workflow-proposal";

const proposal: Proposal = {
  nodes: [
    { id: "p1", name: "brief", type: "ctx", position: { x: 0, y: 0 }, config: {}, wires: [] },
    { id: "p2", name: "worker", type: "agent", position: { x: 1, y: 1 }, config: {}, wires: [] },
  ],
  wires: [{ from: "p1", to: "p2", type: "send" }],
};

const api = (over: Partial<ApplyApi> = {}): ApplyApi => ({
  createNode: vi.fn(async (n) => ({ id: `real-${n.name}` }) as never),
  createWire: vi.fn(async () => ({})),
  ...over,
});

describe("applyProposal", () => {
  it("creates nodes first, then wires, and maps the builder's ids to the engine's", async () => {
    const a = api();
    const r = await applyProposal(a, proposal);
    expect(r.complete).toBe(true);
    expect(r.createdNodes).toEqual([
      { name: "brief", id: "real-brief" },
      { name: "worker", id: "real-worker" },
    ]);
    // The builder's "p1"/"p2" are its own invention; the wire must use what the engine assigned.
    expect(a.createWire).toHaveBeenCalledWith("real-brief", "real-worker", "send");
  });

  it("NEVER reports complete when anything failed", async () => {
    /**
     * The success-shape invariant. A board that half-applies and says "done" leaves the user with a
     * broken board they believe is finished — worse than a plain failure, because they stop looking.
     */
    const a = api({ createWire: vi.fn(async () => { throw new Error("wire refused by the matrix"); }) });
    const r = await applyProposal(a, proposal);
    expect(r.complete).toBe(false);
    expect(r.createdNodes).toHaveLength(2);
    expect(r.failures).toEqual([
      { what: "wire brief → worker (send)", reason: "wire refused by the matrix" },
    ]);
  });

  it("keeps going after a failure, so the report is the WHOLE damage", async () => {
    // Stopping at the first error would hide the rest, and the user needs the full list to clean up.
    const a = api({
      createNode: vi.fn(async (n) => {
        if (n.name === "brief") throw new Error("name taken");
        return { id: `real-${n.name}` } as never;
      }),
    });
    const r = await applyProposal(a, proposal);
    expect(r.createdNodes).toEqual([{ name: "worker", id: "real-worker" }]);
    expect(r.failures.map((f) => f.what)).toEqual(['node "brief"', "wire brief → worker (send)"]);
  });

  it("does not send a wire with a dangling id when its endpoint failed", async () => {
    const createWire = vi.fn(async () => ({}));
    const a = api({
      createNode: vi.fn(async () => { throw new Error("nope"); }),
      createWire,
    });
    const r = await applyProposal(a, proposal);
    expect(createWire).not.toHaveBeenCalled();
    expect(r.failures.at(-1)?.reason).toMatch(/not created/);
  });

  it("reports an empty proposal as complete, because nothing failed", async () => {
    const r = await applyProposal(api(), { nodes: [], wires: [] });
    expect(r).toEqual({ createdNodes: [], createdWires: [], failures: [], complete: true });
  });
});
