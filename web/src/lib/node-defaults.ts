import type { NodeOfType, NodeType, WheelNode } from "@/lib/schema";

type ConfigFor<T extends NodeType> = NodeOfType<T>["config"];

/**
 * The config a node is born with, per §3.
 *
 * The engine requires `config` on POST /v1/nodes — it does not invent one — so whoever places a
 * node has to supply it. This lives here rather than inline at the call site because the mock
 * needs exactly the same answer: when the two disagree, the board works against the mock and
 * 422s against the engine, which is precisely the bug this file was written to close.
 *
 * Typed per node type off the node union, so adding a tenth type fails to compile here rather
 * than shipping a node the engine will reject.
 */
const DEFAULTS: { [T in NodeType]: () => ConfigFor<T> } = {
  agent: () => ({
    harness: "claude",
    system_prompt: "",
    run_on_startup: false,
    ephemeral_context: false,
  }),
  ctx: () => ({ markdown: "" }),
  table: () => ({ columns: [{ name: "value", type: "text" }] }),
  endpoint: () => ({ method: "POST", path: "/hook", response_mode: "ack" }),
  script: () => ({ language: "python", source: "print('hello from wheel')\n", timeout_secs: 60 }),
  mcp: () => ({ transport: "stdio", command: "" }),
  vault: () => ({ keys: [] }),
  chest: () => ({}),
  // A tool is born empty and manual: no document imported yet, no operations, so nothing is
  // exposed to an agent until the person imports a spec and decides who fills what.
  tool: () => ({
    kind: "http",
    base_url: "",
    operations: [],
    source: { format: "manual", imported_at: new Date().toISOString(), raw: "" },
  }),
};

export function defaultConfigFor<T extends NodeType>(type: T): ConfigFor<T> {
  return DEFAULTS[type]();
}

/** A node's config is only meaningful alongside its type; this keeps the pair together. */
export function newNodeInput<T extends NodeType>(
  type: T,
  name: string,
  position: WheelNode["position"],
) {
  return { name, type, position, config: defaultConfigFor(type) };
}
