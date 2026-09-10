// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { ReactFlow } from "@xyflow/react";
import { NodePlate, type PlateData } from "./node-plate";
import type { AgentNode } from "@/lib/schema";

// jsdom has no ResizeObserver; react-flow uses one to measure the pane and every node. Nothing
// here asserts on measurement, only on which handles exist, so a no-op stub is enough.
beforeAll(() => {
  class ResizeObserverStub {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  vi.stubGlobal("ResizeObserver", ResizeObserverStub);
});

afterEach(cleanup);

const node: AgentNode = {
  id: "n1",
  name: "planner",
  type: "agent",
  position: { x: 0, y: 0 },
  wires: [],
  config: { harness: "claude", system_prompt: "", run_on_startup: false, ephemeral_context: false },
  state: { kind: "agent", status: "idle" },
} as AgentNode;

const data: PlateData = {
  node,
  takenNames: [],
  onRename: () => {},
  onOpenLog: () => {},
  onAuthenticate: () => {},
};

/**
 * A node dragged so its facing side is the opposite of the ONE fixed handle it used to have (a
 * left-only target, a right-only source) had nothing to grab on that side at all — the operator
 * hit this dragging tool→vault where both nodes' facing sides were on the right. `wire-edge.tsx`'s
 * rendering was already handle-agnostic (it floats to whichever side faces the other node
 * regardless of which handle made the connection); this is the interactive half of that fix.
 */
describe("NodePlate — a wire can be grabbed from or dropped onto either side", () => {
  it("renders both a source and a target handle on the left AND the right", () => {
    const { container } = render(
      <ReactFlow nodes={[{ id: "n1", type: "plate", position: { x: 0, y: 0 }, data }]} nodeTypes={{ plate: NodePlate }} />,
    );

    const byId = (id: string) => container.querySelector(`[data-handleid="${id}"]`);
    const expectHandle = (id: string, type: "source" | "target", pos: "left" | "right") => {
      const el = byId(id);
      expect(el, `expected a handle with id="${id}"`).not.toBeNull();
      expect(el!.classList.contains(type)).toBe(true);
      expect(el!.getAttribute("data-handlepos")).toBe(pos);
    };

    expectHandle("target-left", "target", "left");
    expectHandle("source-left", "source", "left");
    expectHandle("source-right", "source", "right");
    expectHandle("target-right", "target", "right");

    // Exactly these four — not a fifth left over from a copy/paste mistake, not one dropped.
    expect(container.querySelectorAll(".react-flow__handle")).toHaveLength(4);
  });

  it("gives the two handles on the same side distinct positions, so neither shadows the other", () => {
    const { container } = render(
      <ReactFlow nodes={[{ id: "n1", type: "plate", position: { x: 0, y: 0 }, data }]} nodeTypes={{ plate: NodePlate }} />,
    );
    const left = ["target-left", "source-left"].map(
      (id) => (container.querySelector(`[data-handleid="${id}"]`) as HTMLElement).style.top,
    );
    const right = ["source-right", "target-right"].map(
      (id) => (container.querySelector(`[data-handleid="${id}"]`) as HTMLElement).style.top,
    );
    expect(left[0]).not.toBe(left[1]);
    expect(right[0]).not.toBe(right[1]);
  });
});
