import type {
  AgentConfig,
  ChestConfig,
  CtxConfig,
  EndpointConfig,
  McpConfig,
  NodeType,
  ScriptConfig,
  TableConfig,
  VaultConfig,
  WheelNode,
} from "@/lib/schema";

export type GlyphKey =
  | "agent"
  | "ctx"
  | "table"
  | "endpoint"
  | "script"
  | "mcp"
  | "vault"
  | "chest";

export interface NodeMeta {
  type: NodeType;
  /** What a person calls it. */
  label: string;
  /** One line, written for someone who has never seen Wheel. */
  blurb: string;
  glyph: GlyphKey;
  colorVar: string;
  /** Keyboard shortcut in the palette. */
  hotkey: string;
}

export const NODE_META: Record<NodeType, NodeMeta> = {
  agent: {
    type: "agent",
    label: "Agent",
    blurb: "A Claude Code or Codex process. It reads its wires and acts.",
    glyph: "agent",
    colorVar: "--type-agent",
    hotkey: "a",
  },
  ctx: {
    type: "ctx",
    label: "Context",
    blurb: "Markdown. Wire it to an agent and it is prepended to every prompt.",
    glyph: "ctx",
    colorVar: "--type-ctx",
    hotkey: "c",
  },
  table: {
    type: "table",
    label: "Table",
    blurb: "Rows agents can read and write, keyed by a name they choose.",
    glyph: "table",
    colorVar: "--type-table",
    hotkey: "t",
  },
  endpoint: {
    type: "endpoint",
    label: "Endpoint",
    blurb: "A public URL. Each hit becomes a message, a row, or a script run.",
    glyph: "endpoint",
    colorVar: "--type-endpoint",
    hotkey: "e",
  },
  script: {
    type: "script",
    label: "Script",
    blurb: "Python or TypeScript an agent can call as a tool.",
    glyph: "script",
    colorVar: "--type-script",
    hotkey: "s",
  },
  mcp: {
    type: "mcp",
    label: "MCP server",
    blurb: "Tools from an MCP server, attached to whichever agents you wire.",
    glyph: "mcp",
    colorVar: "--type-mcp",
    hotkey: "m",
  },
  vault: {
    type: "vault",
    label: "Vault",
    blurb: "Secrets. You can set them; nothing reads them back out to you.",
    glyph: "vault",
    colorVar: "--type-vault",
    hotkey: "v",
  },
  chest: {
    type: "chest",
    label: "Chest",
    blurb: "Files. Agents put them here to hand to each other or to you.",
    glyph: "chest",
    colorVar: "--type-chest",
    hotkey: "h",
  },
};

/** Palette order: the two you need for anything, then storage, then edges. */
export const PALETTE_ORDER: NodeType[] = [
  "agent",
  "ctx",
  "table",
  "chest",
  "vault",
  "script",
  "endpoint",
  "mcp",
];

export const DEFAULT_CONFIG: {
  agent: AgentConfig;
  ctx: CtxConfig;
  table: TableConfig;
  endpoint: EndpointConfig;
  script: ScriptConfig;
  mcp: McpConfig;
  vault: VaultConfig;
  chest: ChestConfig;
} = {
  agent: {
    harness: "claude",
    system_prompt: "",
    run_on_startup: false,
    ephemeral_context: false,
  },
  ctx: { markdown: "" },
  table: { columns: [] },
  endpoint: { method: "POST", path: "/hook", response_mode: "ack" },
  script: { language: "python", source: "", timeout_secs: 60 },
  mcp: { transport: "stdio", command: "", args: [] },
  vault: { keys: [] },
  chest: {} as ChestConfig,
};

export function defaultConfigFor(type: NodeType): WheelNode["config"] {
  return structuredClone(DEFAULT_CONFIG[type]) as WheelNode["config"];
}

export const STATUS_META: Record<
  string,
  { label: string; tone: "idle" | "busy" | "live" | "alarm"; hint: string }
> = {
  stopped: { label: "Stopped", tone: "idle", hint: "Not running. Start it to deliver its queued messages." },
  starting: { label: "Starting", tone: "busy", hint: "The engine is spawning the harness." },
  needs_auth: {
    label: "Needs sign-in",
    tone: "alarm",
    hint: "The harness has no credentials yet. Authenticate to continue.",
  },
  running: { label: "Running", tone: "live", hint: "Working on a turn." },
  idle: { label: "Idle", tone: "live", hint: "Up and waiting for a message." },
  error: { label: "Error", tone: "alarm", hint: "The harness exited. Check the log." },
};
