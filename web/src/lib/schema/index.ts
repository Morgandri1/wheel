// HAND-WRITTEN — mirrors docs/ARCHITECTURE.md §3 exactly.
// Replaced by `pnpm gen:types` once SDK exports docs/schema/*.json.

export const NODE_TYPES = [
  "agent",
  "ctx",
  "table",
  "endpoint",
  "script",
  "mcp",
  "vault",
  "chest",
] as const;
export type NodeType = (typeof NODE_TYPES)[number];

export const WIRE_TYPES = ["read", "write", "send"] as const;
export type WireType = (typeof WIRE_TYPES)[number];

export const AGENT_STATUSES = [
  "stopped",
  "starting",
  "needs_auth",
  "running",
  "idle",
  "error",
] as const;
export type AgentStatus = (typeof AGENT_STATUSES)[number];

/** §3: node names are addresses. */
export const NODE_NAME_RE = /^[a-z0-9][a-z0-9-_]{0,62}$/;

export interface Position {
  x: number;
  y: number;
}

export interface Wire {
  /** Target node id. Wires stored on a node are OUTGOING only. */
  to: string;
  type: WireType;
}

export type Harness = "claude" | "codex";

export interface AgentConfig {
  harness: Harness;
  model?: string;
  system_prompt: string;
  run_on_startup: boolean;
  ephemeral_context: boolean;
}

export interface CtxConfig {
  markdown: string;
}

export const COLUMN_TYPES = ["text", "integer", "real", "blob", "json"] as const;
export type ColumnType = (typeof COLUMN_TYPES)[number];

export interface TableColumn {
  name: string;
  type: ColumnType;
}

export interface TableConfig {
  columns: TableColumn[];
}

export const HTTP_METHODS = ["GET", "POST", "PUT", "DELETE"] as const;
export type HttpMethod = (typeof HTTP_METHODS)[number];

export interface EndpointConfig {
  method: HttpMethod;
  /** Leading slash, no `..`. */
  path: string;
  response_mode: "ack" | "script";
}

export const SCRIPT_LANGUAGES = ["python", "ts", "js"] as const;
export type ScriptLanguage = (typeof SCRIPT_LANGUAGES)[number];

export interface ScriptConfig {
  language: ScriptLanguage;
  source: string;
  timeout_secs?: number;
}

export interface McpConfig {
  transport: "stdio" | "http";
  command?: string;
  args?: string[];
  url?: string;
  env?: Record<string, string>;
}

/** Values are write-only; the API never returns them and this client has no getter. */
export interface VaultConfig {
  keys: string[];
}

export type ChestConfig = Record<string, never>;

export type NodeConfigFor<T extends NodeType> = T extends "agent"
  ? AgentConfig
  : T extends "ctx"
    ? CtxConfig
    : T extends "table"
      ? TableConfig
      : T extends "endpoint"
        ? EndpointConfig
        : T extends "script"
          ? ScriptConfig
          : T extends "mcp"
            ? McpConfig
            : T extends "vault"
              ? VaultConfig
              : ChestConfig;

export interface AgentState {
  status: AgentStatus;
  session_id?: string | null;
  last_activity?: string | null;
  last_error?: string | null;
}

interface NodeBase<T extends NodeType> {
  id: string;
  name: string;
  type: T;
  position: Position;
  wires: Wire[];
  config: NodeConfigFor<T>;
  /**
   * Runtime state, reported alongside config by GET /v1/board.
   * ASSUMPTION (see docs/plans/web.md §6 Q1): nested, null for non-agent types.
   */
  state?: AgentState | null;
}

export type AgentNode = NodeBase<"agent">;
export type CtxNode = NodeBase<"ctx">;
export type TableNode = NodeBase<"table">;
export type EndpointNode = NodeBase<"endpoint">;
export type ScriptNode = NodeBase<"script">;
export type McpNode = NodeBase<"mcp">;
export type VaultNode = NodeBase<"vault">;
export type ChestNode = NodeBase<"chest">;

export type WheelNode =
  | AgentNode
  | CtxNode
  | TableNode
  | EndpointNode
  | ScriptNode
  | McpNode
  | VaultNode
  | ChestNode;

export const PROJECT_STATUSES = ["stopped", "starting", "running", "error"] as const;
export type ProjectStatus = (typeof PROJECT_STATUSES)[number];

export interface Project {
  id: string;
  owner_id: string;
  name: string;
  capabilities: { http: boolean };
  status: ProjectStatus;
  created_at: string;
  updated_at: string;
  /** Proposed to PM (§6 Q6); falls back to NEXT_PUBLIC_API_URL when absent. */
  ingress_base_url?: string;
}

export interface Board {
  nodes: WheelNode[];
  project: Project;
}

/** §3c: delivery is observable. `consumed` = the harness reported the turn complete. */
export const MESSAGE_STATES = ["queued", "delivered", "consumed"] as const;
export type MessageState = (typeof MESSAGE_STATES)[number];

/** Mirrors the engine's `messages` row (§3c "Message delivery contract"). */
export interface Message {
  id: string;
  from_node: string;
  to_node: string;
  body: string;
  /** sha256 of the body as sent, so a mangled delivery is visible rather than guessed at. */
  sha256: string;
  bytes: number;
  reply_to?: string | null;
  state: MessageState;
  created_at: string;
  delivered_at?: string | null;
  consumed_at?: string | null;
  /** Set when delivery could not proceed; the message stays queued and is never truncated. */
  last_error?: string | null;
  /** Denormalised by the engine for display. */
  from_name?: string;
  from_type?: NodeType | "user" | "system";
}

export type LogStream = "stdout" | "stderr" | "system";

export interface LogLine {
  node_id: string;
  cursor: string;
  stream: LogStream;
  line: string;
  ts: string;
}

export interface AuthBeginResponse {
  mode: "device_code" | "paste_code" | "api_key";
  url?: string;
  user_code?: string;
  instructions: string;
}

export interface AuthStatus {
  authenticated: boolean;
  account?: string;
}

/** §4 WebSocket frames. ASSUMPTION (§6 Q2) until PROTOCOL.md lands. */
export type EngineEvent =
  | { type: "node.state"; project_id: string; ts: string; node_id: string; state: AgentState }
  | { type: "message"; project_id: string; ts: string; message: Message }
  | { type: "log"; project_id: string; ts: string } & LogLine
  | { type: "board.changed"; project_id: string; ts: string };
