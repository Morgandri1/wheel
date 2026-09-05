import { describe, expect, it } from "vitest";
import { plateSignature } from "@/components/board/node-plate";
import type { AgentNode, WheelNode } from "@/lib/schema";

const agent = (over: Partial<AgentNode> = {}): AgentNode =>
  ({
    id: "a1",
    name: "planner",
    type: "agent",
    position: { x: 0, y: 0 },
    wires: [],
    config: { harness: "claude", system_prompt: "", run_on_startup: false, ephemeral_context: false },
    state: { kind: "agent", status: "idle" },
    ...over,
  }) as AgentNode;

/** A fresh parse of the same board — what every refetch actually hands the component. */
const reparse = <T,>(n: T): T => JSON.parse(JSON.stringify(n)) as T;

describe("plate memo signature", () => {
  it("is stable across a refetch, so a tick does not re-render every plate", () => {
    const node = agent();
    expect(plateSignature(reparse(node))).toBe(plateSignature(node));
    // The bug this replaced: identity comparison is always false after a reparse.
    expect(reparse(node) === node).toBe(false);
  });

  it("changes when the status changes", () => {
    expect(plateSignature(agent({ state: { kind: "agent", status: "running" } }))).not.toBe(
      plateSignature(agent({ state: { kind: "agent", status: "idle" } })),
    );
  });

  it("changes when an error appears, so it is not silently dropped", () => {
    expect(
      plateSignature(agent({ state: { kind: "agent", status: "error", last_error: "boom" } })),
    ).not.toBe(plateSignature(agent({ state: { kind: "agent", status: "error" } })));
  });

  it("changes on rename and on harness, the other two things the plate draws", () => {
    expect(plateSignature(agent({ name: "writer" }))).not.toBe(plateSignature(agent()));
    expect(
      plateSignature(
        agent({
          config: {
            harness: "codex",
            system_prompt: "",
            run_on_startup: false,
            ephemeral_context: false,
          },
        }),
      ),
    ).not.toBe(plateSignature(agent()));
  });

  it("ignores what the plate does not draw — position and the system prompt", () => {
    expect(plateSignature(agent({ position: { x: 999, y: 999 } }))).toBe(plateSignature(agent()));
    expect(
      plateSignature(
        agent({
          config: {
            harness: "claude",
            system_prompt: "a long prompt nobody can see from the board",
            run_on_startup: false,
            ephemeral_context: false,
          },
        }),
      ),
    ).toBe(plateSignature(agent()));
  });

  it("summarises every node type without throwing", () => {
    const nodes: WheelNode[] = [
      agent(),
      { id: "c", name: "ctx", type: "ctx", position: { x: 0, y: 0 }, wires: [], config: { markdown: "hi" } },
      { id: "t", name: "t", type: "table", position: { x: 0, y: 0 }, wires: [], config: { columns: [] } },
      { id: "e", name: "e", type: "endpoint", position: { x: 0, y: 0 }, wires: [], config: { method: "POST", path: "/x", response_mode: "ack" } },
      { id: "s", name: "s", type: "script", position: { x: 0, y: 0 }, wires: [], config: { language: "python", source: "" } },
      { id: "v", name: "v", type: "vault", position: { x: 0, y: 0 }, wires: [], config: { keys: ["a"] } },
      { id: "m", name: "m", type: "mcp", position: { x: 0, y: 0 }, wires: [], config: { transport: "stdio", command: "npx" } },
      { id: "h", name: "h", type: "chest", position: { x: 0, y: 0 }, wires: [], config: {} },
      { id: "o", name: "o", type: "tool", position: { x: 0, y: 0 }, wires: [], config: { kind: "http", base_url: "https://x.dev", operations: [], source: { format: "manual", imported_at: "2026-01-01T00:00:00Z" } } },
    ] as WheelNode[];

    for (const n of nodes) {
      expect(plateSignature(n), n.type).toContain(n.id);
      expect(plateSignature(reparse(n)), n.type).toBe(plateSignature(n));
    }
  });
});
