import { describe, expect, it } from "vitest";
import { boardSizeWarning, planSummary, readOutcome, resultingNodeCount } from "./board-apply";

const wire = { from: "notes", to: "researcher", type: "send" as const };

describe("readOutcome — branch on `applied`, never on the status code", () => {
  it("calls a 207 PARTIAL even though it is a 2xx", () => {
    /**
     * The whole point of the apply step. A client that treats 2xx as fine reports a half-applied
     * board as success, which is the one outcome the feature exists to prevent.
     */
    const o = readOutcome(207, {
      applied: false,
      report: { created_nodes: ["notes"], patched_nodes: [], created_wires: [], failures: [
        { step: "create wire notes -> researcher (send)", error: "engine returned 400", wire },
      ] },
    });
    expect(o.kind).toBe("partial");
    if (o.kind !== "partial") return;
    expect(o.report.failures[0]?.wire).toEqual(wire);
  });

  it("treats a MISSING `applied` as not applied", () => {
    // Claiming success from an absent boolean is the wrong direction to fail in.
    expect(readOutcome(200, { report: { created_nodes: [], patched_nodes: [], created_wires: [], failures: [] } }).kind)
      .toBe("partial");
  });

  it("reads a full apply", () => {
    const o = readOutcome(200, {
      applied: true,
      report: { created_nodes: ["notes"], patched_nodes: [], created_wires: [wire], failures: [] },
    });
    expect(o.kind).toBe("applied");
    if (o.kind === "applied") expect(o.report.created_wires).toEqual([wire]);
  });

  it("reads a dry-run plan, which is not an apply at all", () => {
    const o = readOutcome(200, {
      applied: false,
      plan: { create_nodes: ["notes"], patch_nodes: [], create_wires: [wire] },
    });
    expect(o.kind).toBe("plan");
  });

  it("reads a 422 as refused, keeping every reason", () => {
    const o = readOutcome(422, {
      applied: false,
      message: "the board was refused; nothing was created",
      refusals: [
        { refusal: { code: "wire_not_allowed" }, message: "agent → agent as read is not allowed" },
        { refusal: { code: "duplicate_name" }, message: "two nodes named notes" },
      ],
    });
    expect(o.kind).toBe("refused");
    if (o.kind !== "refused") return;
    // Every refusal, not just the first: one bad builder wire usually means several.
    expect(o.refusals).toHaveLength(2);
    expect(o.refusals[0]?.code).toBe("wire_not_allowed");
  });

  it("drops a wire that does not match the pinned shape instead of guessing its type", () => {
    const o = readOutcome(200, {
      applied: false,
      plan: { create_nodes: [], patch_nodes: [], create_wires: [wire, "notes -> researcher (send)"] },
    });
    expect(o.kind).toBe("plan");
    if (o.kind === "plan") expect(o.plan.create_wires).toEqual([wire]);
  });
});

describe("planSummary", () => {
  it("says what the user is confirming", () => {
    expect(planSummary({ create_nodes: ["a", "b"], patch_nodes: [], create_wires: [wire] }))
      .toBe("create 2 nodes, add 1 wire");
  });

  it("says plainly when a re-apply would change nothing", () => {
    expect(planSummary({ create_nodes: [], patch_nodes: [], create_wires: [] }))
      .toMatch(/already matches/);
  });
});

describe("the plan shows the TOTAL, because the API's cap is per-request", () => {
  /**
   * API's 200-node cap bounds one request, not the project, and §3e's per-project cap is documented
   * but unimplemented — so a board grows without limit an apply at a time. A user approving
   * "create 40 nodes" cannot see they are going from 180 to 220. The delta is what they approve;
   * the total is what they live with.
   */
  const plan = { create_nodes: Array.from({ length: 40 }, (_, i) => `n${i}`), patch_nodes: [], create_wires: [] };

  it("adds the plan to what is already there", () => {
    expect(resultingNodeCount(180, plan)).toBe(220);
    expect(resultingNodeCount(0, plan)).toBe(40);
  });

  it("warns only when the RESULT crosses the board's own limit, not the request's", () => {
    // 40 nodes is far inside API's 200-per-request cap and would be accepted without comment.
    expect(boardSizeWarning(0, plan)).toBeNull();
    expect(boardSizeWarning(180, plan)).toMatch(/220 nodes/);
    expect(boardSizeWarning(180, plan)).toMatch(/nothing on the server stops it/i);
  });
});
