import { describe, expect, it } from "vitest";
import { planSummary, readOutcome } from "./board-apply";

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
