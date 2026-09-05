/**
 * In-memory Wheel, good enough to develop the whole board against.
 *
 * This file is the written form of what web/ assumes the engine (§4) and API (§5) do.
 * Where docs/PROTOCOL.md disagrees, THIS changes — not the contract.
 */
import { randomUUID } from "node:crypto";
import {
  isWireAllowed,
  allowedWireTypes,
} from "../src/lib/wire-matrix";
import type {
  AgentNode,
  AgentState,
  AgentStatus,
  EngineEvent,
  Message,
  NodeType,
  Project,
  WheelNode,
  WireType,
} from "../src/lib/schema";
import { NODE_NAME_RE } from "../src/lib/schema";
import { seedProject, seedNodes } from "./fixtures";

export const MOCK_OWNER = "user_mock";

export class HttpError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

interface ProjectState {
  project: Project;
  nodes: WheelNode[];
  messages: Message[];
  logs: Map<string, { cursor: number; lines: { cursor: string; stream: string; line: string; ts: string }[] }>;
  auth: Map<string, { authenticated: boolean; account?: string; pending?: string }>;
  tables: Map<string, Record<string, unknown>[]>;
  chests: Map<string, Map<string, Buffer>>;
  vaults: Map<string, Set<string>>;
  timers: NodeJS.Timeout[];
}

type Listener = (e: EngineEvent) => void;

const projects = new Map<string, ProjectState>();
const listeners = new Map<string, Set<Listener>>();

/** Set by ?chaos=wire on wire creation to exercise the "engine disagreed with the UI" path. */
export let chaosWire = false;
export function setChaosWire(on: boolean) {
  chaosWire = on;
}

function now() {
  return new Date().toISOString();
}

export function emit(projectId: string, event: EngineEvent) {
  for (const l of listeners.get(projectId) ?? []) l(event);
}

export function subscribe(projectId: string, l: Listener): () => void {
  let set = listeners.get(projectId);
  if (!set) listeners.set(projectId, (set = new Set()));
  set.add(l);
  return () => set!.delete(l);
}

export function init() {
  const st: ProjectState = {
    project: { ...seedProject },
    nodes: seedNodes.map((n) => ({ ...n })),
    messages: [],
    logs: new Map(),
    auth: new Map(),
    tables: new Map(),
    chests: new Map(),
    vaults: new Map(),
    timers: [],
  };
  projects.set(st.project.id, st);
}

export function listProjects(): Project[] {
  return [...projects.values()].map((p) => p.project);
}

export function getState(id: string): ProjectState {
  const st = projects.get(id);
  // §5: non-owned / non-existent projects are indistinguishable.
  if (!st || st.project.owner_id !== MOCK_OWNER) {
    throw new HttpError(404, "not_found", "No such project.");
  }
  return st;
}

export function createProject(name: string): Project {
  if (!name?.trim()) throw new HttpError(400, "invalid_name", "A project needs a name.");
  const id = randomUUID();
  const project: Project = {
    id,
    owner_id: MOCK_OWNER,
    name: name.trim(),
    capabilities: { http: false },
    status: "stopped",
    created_at: now(),
    updated_at: now(),
  };
  projects.set(id, {
    project,
    nodes: [],
    messages: [],
    logs: new Map(),
    auth: new Map(),
    tables: new Map(),
    chests: new Map(),
    vaults: new Map(),
    timers: [],
  });
  return project;
}

export function patchProject(id: string, patch: Partial<Project>): Project {
  const st = getState(id);
  if (patch.name !== undefined) {
    if (!patch.name.trim()) throw new HttpError(400, "invalid_name", "A project needs a name.");
    st.project.name = patch.name.trim();
  }
  if (patch.capabilities !== undefined) {
    st.project.capabilities = { http: Boolean(patch.capabilities.http) };
  }
  st.project.updated_at = now();
  return st.project;
}

export function deleteProject(id: string) {
  const st = getState(id);
  for (const t of st.timers) clearTimeout(t);
  projects.delete(id);
}

export function setProjectStatus(id: string, status: Project["status"], delayMs = 0) {
  const st = getState(id);
  const apply = () => {
    st.project.status = status;
    st.project.updated_at = now();
    emit(id, { type: "board.changed", project_id: id, ts: now() });
  };
  if (delayMs) st.timers.push(setTimeout(apply, delayMs));
  else apply();
}

// ---------------------------------------------------------------- nodes

export function board(id: string) {
  const st = getState(id);
  return { nodes: st.nodes, project: st.project };
}

export function createNode(
  id: string,
  body: { name: string; type: NodeType; position: { x: number; y: number }; config?: unknown },
): WheelNode {
  const st = getState(id);
  if (!NODE_NAME_RE.test(body.name)) {
    throw new HttpError(
      400,
      "invalid_name",
      "Names use lowercase letters, digits, hyphen and underscore, and start with a letter or digit.",
    );
  }
  if (st.nodes.some((n) => n.name === body.name)) {
    throw new HttpError(409, "duplicate_name", `A node named “${body.name}” already exists.`);
  }
  const node = {
    id: randomUUID(),
    name: body.name,
    type: body.type,
    position: body.position,
    wires: [],
    config: body.config ?? defaultConfig(body.type),
    state: body.type === "agent" ? { status: "stopped" as AgentStatus } : null,
  } as WheelNode;
  st.nodes.push(node);
  emit(id, { type: "board.changed", project_id: id, ts: now() });
  return node;
}

export function defaultConfig(type: NodeType): unknown {
  switch (type) {
    case "agent":
      return {
        harness: "claude",
        system_prompt: "",
        run_on_startup: false,
        ephemeral_context: false,
      };
    case "ctx":
      return { markdown: "" };
    case "table":
      return { columns: [{ name: "id", type: "integer" }] };
    case "endpoint":
      return { method: "POST", path: "/hook", response_mode: "ack" };
    case "script":
      return { language: "python", source: "print('hello from wheel')\n", timeout_secs: 60 };
    case "mcp":
      return { transport: "stdio", command: "", args: [] };
    case "vault":
      return { keys: [] };
    default:
      return {};
  }
}

export function findNode(id: string, nodeId: string): WheelNode {
  const st = getState(id);
  const n = st.nodes.find((x) => x.id === nodeId);
  if (!n) throw new HttpError(404, "not_found", "No such node.");
  return n;
}

export function patchNode(id: string, nodeId: string, patch: Record<string, unknown>): WheelNode {
  const st = getState(id);
  const node = findNode(id, nodeId);
  if (typeof patch.name === "string") {
    if (!NODE_NAME_RE.test(patch.name)) {
      throw new HttpError(400, "invalid_name", "Names use lowercase letters, digits, hyphen and underscore.");
    }
    if (st.nodes.some((n) => n.name === patch.name && n.id !== nodeId)) {
      throw new HttpError(409, "duplicate_name", `A node named “${patch.name}” already exists.`);
    }
    node.name = patch.name;
  }
  if (patch.position) node.position = patch.position as { x: number; y: number };
  if (patch.config) node.config = { ...(node.config as object), ...(patch.config as object) } as never;
  if (patch.name || patch.config) emit(id, { type: "board.changed", project_id: id, ts: now() });
  return node;
}

export function deleteNode(id: string, nodeId: string) {
  const st = getState(id);
  const node = findNode(id, nodeId);
  st.nodes = st.nodes.filter((n) => n.id !== nodeId);
  for (const n of st.nodes) n.wires = n.wires.filter((w) => w.to !== nodeId);
  st.tables.delete(nodeId);
  st.chests.delete(nodeId);
  st.logs.delete(nodeId);
  st.auth.delete(nodeId);
  void node;
  emit(id, { type: "board.changed", project_id: id, ts: now() });
}

// ---------------------------------------------------------------- wires

export function createWire(id: string, from: string, to: string, type: WireType) {
  const src = findNode(id, from);
  const dst = findNode(id, to);
  if (from === to) throw new HttpError(400, "self_wire", "A node cannot wire to itself.");

  // chaos mode: reject a wire the client considers legal, so the UI's rejection path is exercised.
  if (chaosWire && src.type === "agent" && dst.type === "ctx") {
    throw new HttpError(403, "wire_denied", "Engine says no: agent → ctx is disabled on this project.");
  }
  if (!isWireAllowed(src.type, dst.type, type)) {
    const legal = allowedWireTypes(src.type, dst.type);
    throw new HttpError(
      403,
      "wire_denied",
      legal.length
        ? `${src.type} → ${dst.type} supports ${legal.join(", ")}, not ${type}.`
        : `Nothing connects ${src.type} to ${dst.type}.`,
    );
  }
  if (src.wires.some((w) => w.to === to && w.type === type)) {
    throw new HttpError(409, "duplicate_wire", "That wire already exists.");
  }
  src.wires.push({ to, type });
  emit(id, { type: "board.changed", project_id: id, ts: now() });
  return { from, to, type };
}

export function deleteWire(id: string, from: string, to: string, type: WireType) {
  const src = findNode(id, from);
  const before = src.wires.length;
  src.wires = src.wires.filter((w) => !(w.to === to && w.type === type));
  if (src.wires.length === before) throw new HttpError(404, "not_found", "No such wire.");
  emit(id, { type: "board.changed", project_id: id, ts: now() });
}

// ---------------------------------------------------------------- agents

function setAgentState(id: string, node: AgentNode, patch: Partial<AgentState>) {
  node.state = { ...(node.state ?? { status: "stopped" }), ...patch };
  emit(id, { type: "node.state", project_id: id, ts: now(), node_id: node.id, state: node.state });
}

export function log(id: string, nodeId: string, stream: "stdout" | "stderr" | "system", line: string) {
  const st = getState(id);
  let l = st.logs.get(nodeId);
  if (!l) st.logs.set(nodeId, (l = { cursor: 0, lines: [] }));
  const entry = { cursor: String(++l.cursor), stream, line, ts: now() };
  l.lines.push(entry);
  if (l.lines.length > 5000) l.lines.splice(0, l.lines.length - 5000);
  emit(id, { type: "log", project_id: id, node_id: nodeId, ...entry } as EngineEvent);
}

export function getLog(id: string, nodeId: string, since?: string) {
  const st = getState(id);
  const l = st.logs.get(nodeId);
  if (!l) return [];
  const from = since ? Number(since) : 0;
  return l.lines.filter((x) => Number(x.cursor) > from).map((x) => ({ node_id: nodeId, ...x }));
}

function requireAgent(id: string, nodeId: string): AgentNode {
  const n = findNode(id, nodeId);
  if (n.type !== "agent") throw new HttpError(400, "not_an_agent", "That node is not an agent.");
  return n;
}

export function startAgent(id: string, nodeId: string) {
  const st = getState(id);
  const agent = requireAgent(id, nodeId);
  if (st.project.status !== "running") {
    throw new HttpError(409, "project_stopped", "Start the project before starting an agent.");
  }
  setAgentState(id, agent, { status: "starting", last_error: null });
  log(id, nodeId, "system", `starting ${agent.config.harness} harness`);
  st.timers.push(
    setTimeout(() => {
      if (!st.auth.get(nodeId)?.authenticated) {
        setAgentState(id, agent, { status: "needs_auth" });
        log(id, nodeId, "stderr", "not authenticated — run the Authenticate flow");
        return;
      }
      setAgentState(id, agent, { status: "running", session_id: randomUUID(), last_activity: now() });
      log(id, nodeId, "system", "session started");
      const injected = injectedCtx(id, agent);
      for (const ctx of injected) {
        log(id, nodeId, "system", `injected context from ${ctx.name} (${ctx.chars} chars)`);
      }
      if (agent.config.system_prompt) log(id, nodeId, "system", "system prompt applied");
      drainQueue(id, agent);
      st.timers.push(
        setTimeout(() => setAgentState(id, agent, { status: "idle" }), 600),
      );
    }, 900),
  );
}

function injectedCtx(id: string, agent: AgentNode) {
  const st = getState(id);
  return st.nodes
    .filter((n) => n.type === "ctx" && n.wires.some((w) => w.to === agent.id && w.type === "send"))
    .map((n) => ({ name: n.name, chars: (n.config as { markdown: string }).markdown.length }));
}

export function stopAgent(id: string, nodeId: string) {
  const agent = requireAgent(id, nodeId);
  setAgentState(id, agent, { status: "stopped", session_id: null });
  log(id, nodeId, "system", "stopped");
}

export function restartAgent(id: string, nodeId: string) {
  stopAgent(id, nodeId);
  const st = getState(id);
  st.timers.push(setTimeout(() => startAgent(id, nodeId), 300));
}

export function clearAgent(id: string, nodeId: string) {
  const agent = requireAgent(id, nodeId);
  log(id, nodeId, "system", "context cleared; re-applying system prompt and injected context");
  setAgentState(id, agent, { session_id: randomUUID() });
  for (const ctx of injectedCtx(id, agent)) {
    log(id, nodeId, "system", `injected context from ${ctx.name} (${ctx.chars} chars)`);
  }
}

export function sendToAgent(id: string, nodeId: string, body: string, fromNode = "user", fromName = "user") {
  const st = getState(id);
  const agent = requireAgent(id, nodeId);
  const msg: Message = {
    id: randomUUID(),
    from_node: fromNode,
    to_node: nodeId,
    body,
    created_at: now(),
    delivered_at: null,
    acked_at: null,
    from_name: fromName,
    from_type: fromNode === "user" ? "user" : "agent",
  };
  st.messages.push(msg);
  emit(id, { type: "message", project_id: id, ts: now(), message: msg });

  const status = agent.state?.status;
  if (status !== "running" && status !== "idle") {
    log(id, nodeId, "system", `queued message from ${fromName} (agent is ${status ?? "stopped"})`);
    return msg;
  }
  deliver(id, agent, msg);
  return msg;
}

function drainQueue(id: string, agent: AgentNode) {
  const st = getState(id);
  for (const m of st.messages.filter((m) => m.to_node === agent.id && !m.delivered_at)) {
    deliver(id, agent, m);
  }
}

function deliver(id: string, agent: AgentNode, msg: Message) {
  const st = getState(id);
  msg.delivered_at = now();
  setAgentState(id, agent, { status: "running", last_activity: now() });
  log(id, agent.id, "system", `[wheel] message from ${msg.from_name} (${msg.from_type}):`);
  for (const line of msg.body.split("\n")) log(id, agent.id, "stdout", line);

  st.timers.push(
    setTimeout(() => {
      const reply = `Understood. Working on: ${msg.body.slice(0, 60)}`;
      for (const chunk of reply.match(/.{1,32}/g) ?? []) log(id, agent.id, "stdout", chunk);
      msg.acked_at = now();

      // Forward to any agent this one can send to, so agent → agent shows up in the UI.
      const target = st.nodes.find(
        (n) => n.type === "agent" && agent.wires.some((w) => w.to === n.id && w.type === "send"),
      );
      if (target && msg.from_node !== target.id) {
        log(id, agent.id, "system", `wheel msg ${target.name} "…"`);
        sendToAgent(id, target.id, `Relayed from ${agent.name}: ${msg.body}`, agent.id, agent.name);
      }

      if (agent.config.ephemeral_context) {
        log(id, agent.id, "system", "ephemeral context: clearing session after turn");
        clearAgent(id, agent.id);
      }
      setAgentState(id, agent, { status: "idle", last_activity: now() });
    }, 1400),
  );
}

export function messages(id: string) {
  return getState(id).messages;
}

// ---------------------------------------------------------------- auth

export function authStatus(id: string, nodeId: string) {
  const st = getState(id);
  const a = st.auth.get(nodeId);
  return { authenticated: Boolean(a?.authenticated), account: a?.account };
}

export function authBegin(id: string, nodeId: string) {
  const st = getState(id);
  const agent = requireAgent(id, nodeId);
  const code = "WHEEL-" + Math.random().toString(36).slice(2, 6).toUpperCase();
  st.auth.set(nodeId, { authenticated: false, pending: code });
  const claude = agent.config.harness === "claude";
  return {
    mode: "device_code" as const,
    url: claude ? "https://claude.ai/device" : "https://auth.openai.com/device",
    user_code: code,
    instructions: `Open the link, enter ${code}, then come back and confirm.`,
  };
}

export function authComplete(id: string, nodeId: string, body: { code?: string; api_key?: string }) {
  const st = getState(id);
  const pending = st.auth.get(nodeId);
  if (body.api_key) {
    if (body.api_key.length < 8) throw new HttpError(400, "invalid_key", "That key looks too short.");
    st.auth.set(nodeId, { authenticated: true, account: "api-key" });
  } else if (body.code) {
    if (pending?.pending && body.code.trim().toUpperCase() !== pending.pending) {
      throw new HttpError(400, "invalid_code", "That code doesn't match the one we issued.");
    }
    st.auth.set(nodeId, { authenticated: true, account: "dev@wheel.local" });
  } else {
    st.auth.set(nodeId, { authenticated: true, account: "dev@wheel.local" });
  }
  log(id, nodeId, "system", "authenticated");
  return authStatus(id, nodeId);
}

// ---------------------------------------------------------------- tables / chests / vaults

export function tableRows(id: string, nodeId: string, limit: number, offset: number) {
  const st = getState(id);
  const rows = st.tables.get(nodeId) ?? [];
  return { rows: rows.slice(offset, offset + limit), total: rows.length };
}

export function tableInsert(id: string, nodeId: string, row: Record<string, unknown>) {
  const st = getState(id);
  const rows = st.tables.get(nodeId) ?? [];
  rows.push(row);
  st.tables.set(nodeId, rows);
  return row;
}

export function tableQuery(id: string, nodeId: string, sql: string) {
  if (!/^\s*select\b/i.test(sql)) {
    throw new HttpError(400, "read_only", "Only SELECT statements run here.");
  }
  const st = getState(id);
  return { rows: (st.tables.get(nodeId) ?? []).slice(0, 200) };
}

export function chestLs(id: string, nodeId: string, prefix = "") {
  const st = getState(id);
  const c = st.chests.get(nodeId) ?? new Map();
  return {
    entries: [...c.entries()]
      .filter(([k]) => k.startsWith(prefix))
      .map(([key, buf]) => ({ key, size: buf.length })),
  };
}

export function chestPut(id: string, nodeId: string, key: string, body: Buffer) {
  if (key.includes("..") || key.startsWith("/")) {
    throw new HttpError(400, "invalid_key", "Keys are relative paths without “..”.");
  }
  if (body.length > 50 * 1024 * 1024) throw new HttpError(413, "too_large", "Blobs are capped at 50 MiB.");
  const st = getState(id);
  let c = st.chests.get(nodeId);
  if (!c) st.chests.set(nodeId, (c = new Map()));
  c.set(key, body);
}

export function chestGet(id: string, nodeId: string, key: string): Buffer {
  const st = getState(id);
  const b = st.chests.get(nodeId)?.get(key);
  if (!b) throw new HttpError(404, "not_found", "No such blob.");
  return b;
}

export function chestDelete(id: string, nodeId: string, key: string) {
  const st = getState(id);
  st.chests.get(nodeId)?.delete(key);
}

/** Write-only by construction: there is no reader for vault values anywhere in this file. */
export function vaultPut(id: string, nodeId: string, key: string, value: string) {
  const st = getState(id);
  const node = findNode(id, nodeId);
  if (node.type !== "vault") throw new HttpError(400, "not_a_vault", "That node is not a vault.");
  if (!value) throw new HttpError(400, "empty_value", "A secret needs a value.");
  let keys = st.vaults.get(nodeId);
  if (!keys) st.vaults.set(nodeId, (keys = new Set()));
  keys.add(key);
  node.config = { keys: [...keys] };
  emit(id, { type: "board.changed", project_id: id, ts: now() });
}
