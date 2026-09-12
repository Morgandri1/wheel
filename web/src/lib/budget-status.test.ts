// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it } from "vitest";
import { budgetLines, readBudgetStatus, type BudgetStatus } from "./budget-status";
import type { AgentNode } from "@/lib/schema";

const agent = (state: Record<string, unknown> | null): AgentNode =>
  ({
    id: "a1",
    name: "planner",
    type: "agent",
    position: { x: 0, y: 0 },
    wires: [],
    config: { harness: "claude", system_prompt: "", run_on_startup: false, ephemeral_context: false },
    state,
  }) as unknown as AgentNode;

describe("readBudgetStatus — the shim ahead of #56's schema regen", () => {
  it("reads a real budget_status off the node's state", () => {
    const status = { max_turns: 10, pct_of_max_turns: 30 };
    expect(readBudgetStatus(agent({ kind: "agent", status: "idle", budget_status: status }))).toEqual(
      status,
    );
  });

  it("returns null when the agent has no budget configured at all", () => {
    expect(readBudgetStatus(agent({ kind: "agent", status: "idle" }))).toBeNull();
  });

  it("collapses an explicit null the same as an absent field", () => {
    expect(readBudgetStatus(agent({ kind: "agent", status: "idle", budget_status: null }))).toBeNull();
  });

  it("does not throw when the node has no state at all", () => {
    expect(readBudgetStatus(agent(null))).toBeNull();
  });
});

describe("budgetLines — one line per ceiling actually configured", () => {
  it("shows only a turns line when only max_turns is set", () => {
    const status: BudgetStatus = { max_turns: 10, pct_of_max_turns: 30 };
    const lines = budgetLines(status, 3, 0);
    expect(lines).toEqual([{ kind: "turns", label: "3 / 10 turns", pct: 30, near: false }]);
  });

  it("shows only a usd line when only max_usd is set", () => {
    const status: BudgetStatus = { max_usd: 10, pct_of_max_usd: 42 };
    const lines = budgetLines(status, 0, 4.2);
    expect(lines).toEqual([{ kind: "usd", label: "$4.20 / $10.00", pct: 42, near: false }]);
  });

  it("shows both lines when both ceilings are set", () => {
    const status: BudgetStatus = { max_turns: 10, pct_of_max_turns: 30, max_usd: 5, pct_of_max_usd: 80 };
    expect(budgetLines(status, 3, 4).map((l) => l.kind)).toEqual(["turns", "usd"]);
  });

  it("flags a line as near the limit at 90% and above, not below", () => {
    expect(budgetLines({ max_turns: 10, pct_of_max_turns: 89.9 }, 9, 0)[0]!.near).toBe(false);
    expect(budgetLines({ max_turns: 10, pct_of_max_turns: 90 }, 9, 0)[0]!.near).toBe(true);
  });

  it("treats a ceiling with an absent percentage as 0%, not a crash", () => {
    expect(budgetLines({ max_turns: 10 }, 0, 0)).toEqual([
      { kind: "turns", label: "0 / 10 turns", pct: 0, near: false },
    ]);
  });

  it("returns no lines for a status with neither ceiling — degrades rather than showing nothing meaningful as something", () => {
    expect(budgetLines({}, 5, 1.5)).toEqual([]);
  });
});
