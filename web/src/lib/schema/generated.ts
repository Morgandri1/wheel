/* eslint-disable */
/**
 * GENERATED — do not edit. Run `pnpm gen:types` after the engine re-exports docs/schema.
 * Source: wheel-core via docs/schema/*.json.
 */
/**
 * RFC3339 UTC timestamp, e.g. 2026-09-05T00:21:00Z
 */

export type Timestamp = string;

/**
 * Lifecycle of an agent node's child process.
 */

export type AgentStatus = "stopped" | "starting" | "needs_auth" | "running" | "idle" | "error";

/**
 * Observed state of an `agent` node.
 */

export interface AgentState {
  last_activity?: Timestamp | null;
  last_error?: string | null;
  /**
   * Messages persisted but not yet delivered into the child.
   */
  queued_messages?: number;
  /**
   * The harness's own session identifier for the current session. Changes on every start and on every `ephemeral_context` clear.
   */
  session_id?: string | null;
  status: AgentStatus;
}

/**
 * How an agent's harness can be authenticated headlessly (`POST /v1/agents/:id/auth/begin`).
 */

export type AuthMode = "device_code" | "paste_code" | "api_key";

export interface AuthBegin {
  /**
   * Human-readable steps for the UI to render verbatim.
   */
  instructions: string;
  mode: AuthMode;
  /**
   * Opaque handle tying `auth/complete` to this `auth/begin`.
   */
  session: string;
  url?: string | null;
  user_code?: string | null;
}

/**
 * Whether an agent's harness currently holds usable credentials (`GET /v1/agents/:id/auth`).
 */

export interface AuthStatus {
  /**
   * Display-only account identifier (e.g. an email). Never a token.
   */
  account?: string | null;
  authenticated: boolean;
}

/**
 * Per-project capabilities, toggled by the owner through the API.
 */

export interface Capabilities {
  /**
   * Public ingress at `/p/<project_id>/*` is served only when this is true.
   */
  http?: boolean;
}

/**
 * The uniform error body used by both the host and the engine.
 */

export interface ErrorBody {
  error: ErrorDetail;
}

export interface ErrorDetail {
  /**
   * Stable machine-readable code, e.g. `wire_denied`, `not_found`.
   */
  code: string;
  message: string;
}

/**
 * Events pushed to `/v1/events` subscribers.
 */

export type Event =
  | {
      node_id: string;
      state: NodeState;
      type: "node.state";
    }
  | {
      message: Message;
      type: "message";
    }
  | {
      line: LogLine;
      type: "log";
    }
  | {
      at: Timestamp;
      type: "board.changed";
    }
  | {
      denial: WireDenial;
      type: "wire.denied";
    };

/**
 * `state` as reported next to a node on `GET /v1/board`. Only agent nodes currently carry state; the enum leaves room for others (e.g. table row counts) without a breaking change.
 */

export type NodeState = {
  kind: "agent";
  last_activity?: Timestamp | null;
  last_error?: string | null;
  /**
   * Messages persisted but not yet delivered into the child.
   */
  queued_messages?: number;
  /**
   * The harness's own session identifier for the current session. Changes on every start and on every `ephemeral_context` clear.
   */
  session_id?: string | null;
  status: AgentStatus;
};

/**
 * RFC3339 UTC timestamp, e.g. 2026-09-05T00:21:00Z
 */

/**
 * Lifecycle of an agent node's child process.
 */

/**
 * Who sent a message. The `type` rendered into the envelope comes from here and is **engine-generated** — a body can never forge it (§3c#5).
 */

export type MessageSender =
  | {
      id: string;
      kind: "node";
      name: NodeName;
      type: NodeType;
    }
  | {
      kind: "user";
    }
  | {
      kind: "system";
    };

/**
 * A node's unique, addressable name. Reserved: user, wheel, system, engine.
 */

export type NodeName = string;

/**
 * The eight node types.
 */

export type NodeType =
  "agent" | "ctx" | "table" | "endpoint" | "script" | "mcp" | "vault" | "chest" | "tool";

/**
 * Delivery state (§3c#4). Strictly forward-moving.
 */

export type MessageState = "queued" | "delivered" | "consumed";

/**
 * Where a log line came from.
 */

export type LogStream = ("stdout" | "stderr") | "engine";

/**
 * What a wire permits. Wires are directional and stored on the *source* node.
 */

export type WireType = "read" | "write" | "send";

/**
 * A message row, persisted before any delivery is attempted.
 */

export interface Message {
  body: string;
  /**
   * Byte length of `body`.
   */
  bytes: number;
  consumed_at?: Timestamp | null;
  created_at: Timestamp;
  delivered_at?: Timestamp | null;
  from: MessageSender;
  id: string;
  /**
   * Why this message is still `queued`. Never a reason to truncate it (§3c#11).
   */
  last_error?: string | null;
  /**
   * Threading (§3c#9): the message this one replies to.
   */
  reply_to?: string | null;
  /**
   * Lowercase hex SHA-256 of `body` as sent, so the sender can prove what arrived is what was sent (§3c#3).
   */
  sha256: string;
  state: MessageState;
  /**
   * Target agent node id.
   */
  to: string;
}

/**
 * One line of agent output. `seq` is monotonic per agent and is the cursor used by `GET /v1/agents/:id/log?since=<seq>`.
 */

export interface LogLine {
  at: Timestamp;
  node_id: string;
  seq: number;
  stream: LogStream;
  text: string;
}

/**
 * A denied capability check, surfaced so the UI can show *why* an agent's call failed and so red-team/QA can assert on it.
 */

export interface WireDenial {
  at: Timestamp;
  from: string;
  reason: string;
  required: WireType;
  /**
   * Target as the caller named it — may not resolve to a node at all.
   */
  target: string;
}

/**
 * Which sandbox implementation the host is running.
 */

export type SandboxBackend = "docker" | "process";

/**
 * `GET /host/v1/healthz`
 */

export interface HostHealth {
  ok: boolean;
  projects_running: number;
  sandbox_backend: SandboxBackend;
}

/**
 * RFC3339 UTC timestamp, e.g. 2026-09-05T00:21:00Z
 */

/**
 * Where a log line came from.
 */

/**
 * One line of agent output. `seq` is monotonic per agent and is the cursor used by `GET /v1/agents/:id/log?since=<seq>`.
 */

/**
 * RFC3339 UTC timestamp, e.g. 2026-09-05T00:21:00Z
 */

/**
 * Who sent a message. The `type` rendered into the envelope comes from here and is **engine-generated** — a body can never forge it (§3c#5).
 */

/**
 * A node's unique, addressable name. Reserved: user, wheel, system, engine.
 */

/**
 * The eight node types.
 */

/**
 * Delivery state (§3c#4). Strictly forward-moving.
 */

/**
 * A message row, persisted before any delivery is attempted.
 */

/**
 * Per-type configuration, adjacently tagged so that it serializes as the contract's `"type": <t>, "config": {...}` pair.
 */

export type NodeConfig =
  | {
      config: AgentConfig;
      type: "agent";
    }
  | {
      config: CtxConfig;
      type: "ctx";
    }
  | {
      config: TableConfig;
      type: "table";
    }
  | {
      config: EndpointConfig;
      type: "endpoint";
    }
  | {
      config: ScriptConfig;
      type: "script";
    }
  | {
      config: McpConfig;
      type: "mcp";
    }
  | {
      config: VaultConfig;
      type: "vault";
    }
  | {
      config: ChestConfig;
      type: "chest";
    }
  | {
      config: ToolConfig;
      type: "tool";
    };

/**
 * Which CLI backs an agent node.
 */

export type Harness = "claude" | "codex";

/**
 * A sqlite-safe identifier (table column name).
 */

export type Ident = string;

/**
 * Column type of a `table` node, mapped onto a sqlite storage class.
 */

export type ColumnType = ("text" | "integer" | "real" | "blob") | "json";

export type HttpMethod = "GET" | "POST" | "PUT" | "DELETE";

/**
 * What an endpoint returns to the HTTP caller.
 */

export type ResponseMode = "ack" | "script";

export type ScriptLanguage = "python" | "ts" | "js";

export type McpTransport = "stdio" | "http";

/**
 * Methods a tool operation may use. Deliberately a separate enum from [`crate::node::HttpMethod`]: `endpoint` nodes are contractually limited to GET/POST/PUT/DELETE (§3), while imported specs routinely contain PATCH and HEAD. Sharing one enum would silently widen the endpoint contract.
 */

export type ToolMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS";

/**
 * How a field is filled when the operation is called (§3d).
 */

export type FillMode = "agent" | "static" | "vault" | "hidden";

/**
 * Where a parameter goes in the HTTP request.
 */

export type ParamLocation = "header" | "path" | "query" | "cookie" | "body";

/**
 * The document format a tool node was imported from.
 */

export type ToolFormat = "openapi3" | "swagger2" | "postman21" | "insomnia4";

export interface AgentConfig {
  /**
   * Clear the session after every completed turn, re-applying the system prompt and ctx injections, before draining the next queued message.
   */
  ephemeral_context?: boolean;
  harness: Harness;
  /**
   * Harness-specific model id. `None` = the CLI's own default.
   */
  model?: string | null;
  /**
   * Start this agent when the container starts.
   */
  run_on_startup?: boolean;
  /**
   * Appended to the harness's own system prompt, then followed by the markdown of every `ctx` node wired `send` into this agent.
   */
  system_prompt: string;
}

export interface CtxConfig {
  markdown: string;
}

export interface TableConfig {
  columns: Column[];
}

export interface Column {
  /**
   * Validated with a sqlite-safe charset so it is safe to quote into DDL. Unlike a node name this may be `user`, `system`, ... — the node reserved-name list is about message addressing, not columns.
   */
  name: Ident;
  type: ColumnType;
}

export interface EndpointConfig {
  method: HttpMethod;
  /**
   * Leading slash, no `..`. Validated by [`crate::validate::validate_endpoint_path`].
   */
  path: string;
  response_mode: ResponseMode;
}

export interface ScriptConfig {
  language: ScriptLanguage;
  source: string;
  timeout_secs?: number | null;
}

export interface McpConfig {
  args?: string[] | null;
  command?: string | null;
  env?: {
    [k: string]: string;
  } | null;
  transport: McpTransport;
  url?: string | null;
}

/**
 * Vault config carries only the *key names*. Values are write-only through `PUT /v1/vault/:id/:key`, stored encrypted, and never returned by `GET /v1/board`.
 */

export interface VaultConfig {
  keys: string[];
}

/**
 * Chest has no configuration; its content lives on disk under `/data/chest/<node_id>/` and is indexed in sqlite.
 */

export interface ChestConfig {}

export interface ToolConfig {
  /**
   * Absolute `http(s)` origin every operation is resolved against.
   */
  base_url: string;
  operations?: ToolOperation[];
  source_format?: ToolFormat | null;
}

/**
 * One callable operation. Exposed over MCP as `<tool name>__<id>`.
 */

export interface ToolOperation {
  enabled?: boolean;
  /**
   * Stable identifier, unique within the node. Charset is restricted because it is concatenated into an MCP tool name.
   */
  id: string;
  method: ToolMethod;
  params?: ToolParam[];
  /**
   * Path template relative to `base_url`, e.g. `/users/{id}`.
   */
  path: string;
  summary?: string | null;
}

export interface ToolParam {
  description?: string | null;
  fill?: Fill;
  location: ParamLocation;
  name: string;
  required?: boolean;
  /**
   * JSON Schema fragment for this field, used to build the MCP input schema.
   */
  schema?: {
    [k: string]: unknown;
  };
}

/**
 * How one field is filled. `value`/`vault_ref` are meaningful only for their corresponding mode; [`crate::validate::validate_config`] enforces that.
 */

export interface Fill {
  mode: FillMode;
  value?: string | null;
  /**
   * `<vault node name>/<key>`.
   */
  vault_ref?: string | null;
}

/**
 * `state` as reported next to a node on `GET /v1/board`. Only agent nodes currently carry state; the enum leaves room for others (e.g. table row counts) without a breaking change.
 */

/**
 * RFC3339 UTC timestamp, e.g. 2026-09-05T00:21:00Z
 */

/**
 * Lifecycle of an agent node's child process.
 */

/**
 * The eight node types.
 */

/**
 * A node plus its observed state, as returned by `GET /v1/board`.
 */

export type NodeWithState = {
  id: string;
  name: NodeName;
  position: Position;
  state?: NodeState | null;
  /**
   * OUTGOING wires only.
   */
  wires?: Wire[];
} & (
  | {
      config: AgentConfig;
      type: "agent";
    }
  | {
      config: CtxConfig;
      type: "ctx";
    }
  | {
      config: TableConfig;
      type: "table";
    }
  | {
      config: EndpointConfig;
      type: "endpoint";
    }
  | {
      config: ScriptConfig;
      type: "script";
    }
  | {
      config: McpConfig;
      type: "mcp";
    }
  | {
      config: VaultConfig;
      type: "vault";
    }
  | {
      config: ChestConfig;
      type: "chest";
    }
  | {
      config: ToolConfig;
      type: "tool";
    }
);

/**
 * A node's unique, addressable name. Reserved: user, wheel, system, engine.
 */

/**
 * `state` as reported next to a node on `GET /v1/board`. Only agent nodes currently carry state; the enum leaves room for others (e.g. table row counts) without a breaking change.
 */

/**
 * RFC3339 UTC timestamp, e.g. 2026-09-05T00:21:00Z
 */

/**
 * Lifecycle of an agent node's child process.
 */

/**
 * What a wire permits. Wires are directional and stored on the *source* node.
 */

/**
 * Which CLI backs an agent node.
 */

/**
 * A sqlite-safe identifier (table column name).
 */

/**
 * Column type of a `table` node, mapped onto a sqlite storage class.
 */

/**
 * What an endpoint returns to the HTTP caller.
 */

/**
 * Methods a tool operation may use. Deliberately a separate enum from [`crate::node::HttpMethod`]: `endpoint` nodes are contractually limited to GET/POST/PUT/DELETE (§3), while imported specs routinely contain PATCH and HEAD. Sharing one enum would silently widen the endpoint contract.
 */

/**
 * How a field is filled when the operation is called (§3d).
 */

/**
 * Where a parameter goes in the HTTP request.
 */

/**
 * The document format a tool node was imported from.
 */

/**
 * Board coordinates. Floats because the canvas pans/zooms continuously.
 */

export interface Position {
  x: number;
  y: number;
}

/**
 * An outgoing wire, as stored on its source node (`Node::wires`).
 */

export interface Wire {
  /**
   * Target node id.
   */
  to: string;
  type: WireType;
}

/**
 * Vault config carries only the *key names*. Values are write-only through `PUT /v1/vault/:id/:key`, stored encrypted, and never returned by `GET /v1/board`.
 */

/**
 * Chest has no configuration; its content lives on disk under `/data/chest/<node_id>/` and is indexed in sqlite.
 */

/**
 * One callable operation. Exposed over MCP as `<tool name>__<id>`.
 */

/**
 * How one field is filled. `value`/`vault_ref` are meaningful only for their corresponding mode; [`crate::validate::validate_config`] enforces that.
 */

/**
 * A board node.
 *
 * `type` and `config` come from the flattened [`NodeConfig`]; [`Node::node_type`] reads the tag back.
 */

export type Node = {
  id: string;
  name: NodeName;
  position: Position;
  /**
   * OUTGOING wires only.
   */
  wires?: Wire[];
} & (
  | {
      config: AgentConfig;
      type: "agent";
    }
  | {
      config: CtxConfig;
      type: "ctx";
    }
  | {
      config: TableConfig;
      type: "table";
    }
  | {
      config: EndpointConfig;
      type: "endpoint";
    }
  | {
      config: ScriptConfig;
      type: "script";
    }
  | {
      config: McpConfig;
      type: "mcp";
    }
  | {
      config: VaultConfig;
      type: "vault";
    }
  | {
      config: ChestConfig;
      type: "chest";
    }
  | {
      config: ToolConfig;
      type: "tool";
    }
);

/**
 * A node's unique, addressable name. Reserved: user, wheel, system, engine.
 */

/**
 * What a wire permits. Wires are directional and stored on the *source* node.
 */

/**
 * Which CLI backs an agent node.
 */

/**
 * A sqlite-safe identifier (table column name).
 */

/**
 * Column type of a `table` node, mapped onto a sqlite storage class.
 */

/**
 * What an endpoint returns to the HTTP caller.
 */

/**
 * Methods a tool operation may use. Deliberately a separate enum from [`crate::node::HttpMethod`]: `endpoint` nodes are contractually limited to GET/POST/PUT/DELETE (§3), while imported specs routinely contain PATCH and HEAD. Sharing one enum would silently widen the endpoint contract.
 */

/**
 * How a field is filled when the operation is called (§3d).
 */

/**
 * Where a parameter goes in the HTTP request.
 */

/**
 * The document format a tool node was imported from.
 */

/**
 * Board coordinates. Floats because the canvas pans/zooms continuously.
 */

/**
 * An outgoing wire, as stored on its source node (`Node::wires`).
 */

/**
 * Vault config carries only the *key names*. Values are write-only through `PUT /v1/vault/:id/:key`, stored encrypted, and never returned by `GET /v1/board`.
 */

/**
 * Chest has no configuration; its content lives on disk under `/data/chest/<node_id>/` and is indexed in sqlite.
 */

/**
 * One callable operation. Exposed over MCP as `<tool name>__<id>`.
 */

/**
 * How one field is filled. `value`/`vault_ref` are meaningful only for their corresponding mode; [`crate::validate::validate_config`] enforces that.
 */

/**
 * Board coordinates. Floats because the canvas pans/zooms continuously.
 */

/**
 * RFC3339 UTC timestamp, e.g. 2026-09-05T00:21:00Z
 */

/**
 * Lifecycle of a project's sandbox. Mirrors `Project.status` in §5.
 */

export type SandboxStatus = "stopped" | "starting" | "running" | "error";

/**
 * `GET /host/v1/projects/:id`
 */

export interface SandboxInfo {
  capabilities?: Capabilities;
  id: string;
  last_error?: string | null;
  started_at?: Timestamp | null;
  status: SandboxStatus;
}

/**
 * Per-project capabilities, toggled by the owner through the API.
 */

/**
 * `PUT /host/v1/projects/:id` — idempotent create-or-update of a sandbox record. The API holds these secrets encrypted in Postgres and hands them to the host here; the host is the only process that has them at runtime.
 */

export interface SandboxUpsert {
  capabilities?: Capabilities;
  /**
   * Bearer the host must present to this project's engine control plane.
   */
  engine_secret: string;
  /**
   * Base64 per-project key the engine uses to encrypt vault values at rest.
   */
  vault_key: string;
}

/**
 * Per-project capabilities, toggled by the owner through the API.
 */

/**
 * Methods a tool operation may use. Deliberately a separate enum from [`crate::node::HttpMethod`]: `endpoint` nodes are contractually limited to GET/POST/PUT/DELETE (§3), while imported specs routinely contain PATCH and HEAD. Sharing one enum would silently widen the endpoint contract.
 */

/**
 * How a field is filled when the operation is called (§3d).
 */

/**
 * Where a parameter goes in the HTTP request.
 */

/**
 * The document format a tool node was imported from.
 */

/**
 * One callable operation. Exposed over MCP as `<tool name>__<id>`.
 */

/**
 * How one field is filled. `value`/`vault_ref` are meaningful only for their corresponding mode; [`crate::validate::validate_config`] enforces that.
 */

/**
 * Methods a tool operation may use. Deliberately a separate enum from [`crate::node::HttpMethod`]: `endpoint` nodes are contractually limited to GET/POST/PUT/DELETE (§3), while imported specs routinely contain PATCH and HEAD. Sharing one enum would silently widen the endpoint contract.
 */

/**
 * How a field is filled when the operation is called (§3d).
 */

/**
 * Where a parameter goes in the HTTP request.
 */

/**
 * One callable operation. Exposed over MCP as `<tool name>__<id>`.
 */

/**
 * How one field is filled. `value`/`vault_ref` are meaningful only for their corresponding mode; [`crate::validate::validate_config`] enforces that.
 */

/**
 * What a wire permits. Wires are directional and stored on the *source* node.
 */

/**
 * A wire including its source, used by API/engine payloads (`POST /v1/wires {from,to,type}`).
 */

export interface WireSpec {
  from: string;
  to: string;
  type: WireType;
}

/**
 * What a wire permits. Wires are directional and stored on the *source* node.
 */

/**
 * What a wire permits. Wires are directional and stored on the *source* node.
 */

/**
 * An outgoing wire, as stored on its source node (`Node::wires`).
 */
