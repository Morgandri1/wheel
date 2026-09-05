import type { AgentStatus, NodeType, WireType } from "@/lib/schema";

/**
 * One place that knows what each node type looks like and is called.
 *
 * Glyphs are drawn on a 16×16 grid at 1.4px stroke — schematic marks, not icons from a set.
 * Type tints are low-chroma on purpose: saturated colour is reserved for wires, so a node
 * plate never competes with the connection running through it.
 */
export interface NodeMeta {
  type: NodeType;
  label: string;
  /** What this node is, in the person's terms — shown in the palette and on empty inspectors. */
  blurb: string;
  tint: string;
  glyph: string;
}

export const NODE_META: Record<NodeType, NodeMeta> = {
  agent: {
    type: "agent",
    label: "Agent",
    blurb: "A Claude or Codex process. Give it a prompt, wire it to what it may touch.",
    tint: "var(--t-agent)",
    glyph: "M4 5h8v6H4z M6 5V3 M10 5V3 M6 13v-2 M10 13v-2 M2 7h2 M2 9h2 M12 7h2 M12 9h2",
  },
  ctx: {
    type: "ctx",
    label: "Context",
    blurb: "Markdown that gets prepended to an agent's prompt every time it starts.",
    tint: "var(--t-ctx)",
    glyph: "M4 2h6l2 2v10H4z M10 2v2h2 M6 7h4 M6 9.5h4 M6 12h2",
  },
  table: {
    type: "table",
    label: "Table",
    blurb: "A SQL table agents can query, and write to if you let them.",
    tint: "var(--t-table)",
    glyph: "M2.5 3h11v10h-11z M2.5 6.5h11 M2.5 9.75h11 M6.5 3v10 M10 3v10",
  },
  endpoint: {
    type: "endpoint",
    label: "Endpoint",
    blurb: "A public URL. Each hit becomes a message, a row, or a script run.",
    tint: "var(--t-endpoint)",
    glyph: "M1.5 8h7 M6 5.5 8.5 8 6 10.5 M11 3h3.5v10H11",
  },
  script: {
    type: "script",
    label: "Script",
    blurb: "Python, TypeScript or JavaScript an agent can call like a tool.",
    tint: "var(--t-script)",
    glyph: "M2.5 3h11v10h-11z M5 6.5 7 8.5 5 10.5 M8.5 10.5h3",
  },
  mcp: {
    type: "mcp",
    label: "MCP server",
    blurb: "An MCP server attached to an agent's harness at its next start.",
    tint: "var(--t-mcp)",
    glyph: "M5.5 2v3.5 M10.5 2v3.5 M3.5 5.5h9v3a4.5 4.5 0 0 1-9 0z M8 13v1.5",
  },
  vault: {
    type: "vault",
    label: "Vault",
    blurb: "Secrets. You set them; you never read them back. Agents get them as env vars.",
    tint: "var(--t-vault)",
    glyph: "M4 7V5a4 4 0 0 1 8 0v2 M2.5 7h11v7h-11z M8 9.5v2",
  },
  chest: {
    type: "chest",
    label: "Chest",
    blurb: "Files. Agents list, read, and — with a write wire — put and remove them.",
    tint: "var(--t-chest)",
    glyph: "M2 5.5 8 2.5l6 3v7L8 15.5l-6-3z M2 5.5 8 8.5l6-3 M8 8.5v7",
  },
  tool: {
    type: "tool",
    label: "Tool",
    blurb:
      "An imported API spec. You decide which fields the agent fills and which come from a vault.",
    tint: "var(--t-tool)",
    glyph: "M10.5 2.2a3.6 3.6 0 0 0-4.3 4.6L2.6 10.4a1.5 1.5 0 0 0 2.1 2.1l3.6-3.6a3.6 3.6 0 0 0 4.6-4.3l-2.1 2.1-1.9-.5-.5-1.9z",
  },
};

/** Palette order: the two you need for a first board, then the rest by how often they're reached for. */
export const PALETTE_ORDER: NodeType[] = [
  "agent",
  "ctx",
  "table",
  "endpoint",
  "script",
  "mcp",
  "vault",
  "chest",
  "tool",
];

export const WIRE_META: Record<WireType, { label: string; color: string; dash: string }> = {
  read: { label: "read", color: "var(--wire-read)", dash: "0" },
  write: { label: "write", color: "var(--wire-write)", dash: "0" },
  send: { label: "send", color: "var(--wire-send)", dash: "5 4" },
};

export const AGENT_STATUS_META: Record<AgentStatus, { label: string; color: string; pulse: boolean }> = {
  stopped: { label: "Stopped", color: "var(--ink-faint)", pulse: false },
  starting: { label: "Starting", color: "var(--wire-write)", pulse: true },
  needs_auth: { label: "Needs sign-in", color: "var(--danger)", pulse: false },
  running: { label: "Running", color: "var(--live)", pulse: true },
  idle: { label: "Idle", color: "var(--live)", pulse: false },
  error: { label: "Error", color: "var(--danger)", pulse: false },
};
