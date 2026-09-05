/**
 * The board's vocabulary, re-exported from the engine's own schema export.
 *
 * Everything structural comes from ./generated, which `pnpm gen:types` builds from
 * docs/schema/*.json (itself produced by wheel-core). Nothing here restates a shape the engine
 * already describes — a second hand-written copy is a divergence waiting to happen, and we have
 * already been bitten by one.
 *
 * What stays hand-written is only what a JSON Schema cannot give TypeScript: runtime value
 * arrays to iterate and render with, and a few aliases that read better at the call site.
 */
export type {
  AgentConfig,
  AgentState,
  AgentStatus,
  AuthBegin,
  AuthMode,
  AuthStatus,
  Capabilities,
  ChestConfig,
  Column,
  ColumnType,
  CtxConfig,
  EndpointConfig,
  ErrorBody,
  ErrorDetail,
  Fill,
  FillMode,
  Harness,
  HostHealth,
  HttpMethod,
  LogLine,
  LogStream,
  McpConfig,
  Message,
  MessageSender,
  MessageState,
  Node,
  NodeConfig,
  NodeName,
  NodeState,
  NodeType,
  NodeWithState,
  ParamLocation,
  Position,
  ResponseMode,
  ScriptConfig,
  ScriptLanguage,
  TableConfig,
  Timestamp,
  ToolConfig,
  ToolFormat,
  ToolMethod,
  ToolOperation,
  ToolParam,
  VaultConfig,
  Wire,
  WireDenial,
  WireSpec,
  WireType,
} from "./generated";

import type {
  Event as GeneratedEvent,
  LogStream,
  NodeType as GeneratedNodeType,
  NodeWithState,
  WireType as GeneratedWireType,
} from "./generated";

/** The engine calls it `Event`; on this side it is always the engine's event, never a DOM one. */
export type EngineEvent = GeneratedEvent;

/** A node as `GET /v1/board` returns it: config plus runtime state. */
export type WheelNode = NodeWithState;

export type NodeOfType<T extends GeneratedNodeType> = Extract<NodeWithState, { type: T }>;

export type AgentNode = NodeOfType<"agent">;
export type CtxNode = NodeOfType<"ctx">;
export type TableNode = NodeOfType<"table">;
export type EndpointNode = NodeOfType<"endpoint">;
export type ScriptNode = NodeOfType<"script">;
export type McpNode = NodeOfType<"mcp">;
export type VaultNode = NodeOfType<"vault">;
export type ChestNode = NodeOfType<"chest">;
export type ToolNode = NodeOfType<"tool">;

/** Iteration order for the palette and for exhaustive tests. Kept in step by the conformance test. */
export const NODE_TYPES = [
  "agent",
  "ctx",
  "table",
  "endpoint",
  "script",
  "mcp",
  "vault",
  "chest",
  "tool",
] as const satisfies readonly GeneratedNodeType[];

export const WIRE_TYPES = ["read", "write", "send"] as const satisfies readonly GeneratedWireType[];

export const AGENT_STATUSES = [
  "stopped",
  "starting",
  "needs_auth",
  "running",
  "idle",
  "parked",
  "budget_exhausted",
  "error",
] as const;

export const COLUMN_TYPES = ["text", "integer", "real", "blob", "json"] as const;
export const HTTP_METHODS = ["GET", "POST", "PUT", "DELETE"] as const;
export const SCRIPT_LANGUAGES = ["python", "ts", "js"] as const;
export const TOOL_FORMATS = ["openapi", "swagger2", "postman", "insomnia", "manual"] as const;
export const PARAM_LOCATIONS = ["path", "query", "header", "cookie"] as const;

/** §3: a node's name is the address other agents send to. */
export const NODE_NAME_RE = /^[a-z0-9][a-z0-9-_]{0,62}$/;

/** Web-side only: the API's project record (§5), which is not part of the engine's schema. */
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
  ingress_base_url?: string;
}

export interface Board {
  nodes: WheelNode[];
  project: Project;
}

/**
 * The engine's own event union. `lagged` means the socket dropped frames rather than let a slow
 * reader stall the delivery loop: the connection is healthy and what you hold is stale, so it is
 * a resync instruction, not an error.
 */
export type EngineFrame = EngineEvent;

/** Log streams, including `transcript` — the exact bytes written to a child's stdin (§3c #10). */
export type LogStreamName = LogStream;
