/**
 * In-memory board that behaves the way docs/ARCHITECTURE.md §4 says an engine
 * behaves. This file is the written form of what the web app ASSUMES about the
 * engine — when PROTOCOL.md disagrees, this changes, not the contract.
 */
import { createHash, randomUUID } from "node:crypto";
import { engineAllowsWire } from "./engine-matrix";
import { defaultConfigFor } from "@/lib/node-defaults";
import { deliveryOrder, senderKind } from "@/lib/message-state";
import type {
  AgentNode,
  AgentStatus,
  EngineEvent,
  LogLine,
  Message,
  MessageSender,
  NodeState,
  NodeType,
  Position,
  Project,
  WheelNode,
  WireType,
} from "@/lib/schema";

export const now = () => new Date().toISOString();

let seq = 0;
const nextSeq = () => ++seq;

export interface ProjectRecord {
  project: Project;
  nodes: WheelNode[];
  messages: Message[];
  log: LogLine[];
  authenticated: Set<string>;
  tables: Map<string, Map<string, Record<string, unknown>>>;
  chests: Map<string, Map<string, Buffer>>;
  vaults: Map<string, Set<string>>;
  listeners: Set<(event: EngineEvent) => void>;
  timers: Set<ReturnType<typeof setTimeout>>;
}

export const projects = new Map<string, ProjectRecord>();

export const OWNER = "user_mock";

export function emit(record: ProjectRecord, event: EngineEvent) {
  for (const listener of record.listeners) listener(event);
}

export function appendLog(
  record: ProjectRecord,
  nodeId: string,
  stream: LogLine["stream"],
  line: string,
) {
  const entry: LogLine = { node_id: nodeId, seq: nextSeq(), stream, text: line, at: now() };
  record.log.push(entry);
  if (record.log.length > 5000) record.log.splice(0, record.log.length - 5000);
  emit(record, { type: "log", line: entry });
  return entry;
}

export function setAgentState(record: ProjectRecord, node: AgentNode, patch: Partial<Omit<NodeState, "kind">>) {
  const state: NodeState = { kind: "agent", status: "stopped", ...node.state, ...patch };
  node.state = state;
  emit(record, { type: "node.state", node_id: node.id, state });
}

export function boardChanged(record: ProjectRecord) {
  emit(record, { type: "board.changed", at: now() });
}

export function findNode(record: ProjectRecord, id: string) {
  return record.nodes.find((n) => n.id === id);
}

export function nodeByName(record: ProjectRecord, name: string) {
  return record.nodes.find((n) => n.name === name);
}

export class EngineRefusal extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

/** §3, enforced from the engine's export — the UI's copy is never load-bearing here. */
export function assertWireLegal(from: WheelNode, to: WheelNode, type: WireType) {
  if (from.id === to.id) throw new EngineRefusal(400, "a node cannot wire to itself");
  if (!engineAllowsWire(from.type, to.type, type)) {
    throw new EngineRefusal(
      400,
      `no wire allowed from ${from.type} to ${to.type} (type: ${type}) — see the wire matrix`,
    );
  }
}

export function createProject(name: string): ProjectRecord {
  const id = randomUUID();
  const record: ProjectRecord = {
    project: {
      id,
      owner_id: OWNER,
      name,
      capabilities: { http: false },
      status: "stopped",
      created_at: now(),
      updated_at: now(),
    },
    nodes: [],
    messages: [],
    log: [],
    authenticated: new Set(),
    tables: new Map(),
    chests: new Map(),
    vaults: new Map(),
    listeners: new Set(),
    timers: new Set(),
  };
  projects.set(id, record);
  return record;
}

export function makeNode(
  type: NodeType,
  name: string,
  position: Position,
  config: unknown,
): WheelNode {
  const base = {
    id: randomUUID(),
    name,
    type,
    position,
    wires: [],
    config: config ?? defaultConfigFor(type),
  } as unknown as WheelNode;
  if (type === "agent") (base as AgentNode).state = { kind: "agent", status: "stopped" };
  return base;
}

const later = (record: ProjectRecord, ms: number, fn: () => void) => {
  const t = setTimeout(() => {
    record.timers.delete(t);
    fn();
  }, ms);
  record.timers.add(t);
};

/** A convincing-enough harness: starting → needs_auth | idle, turns, replies. */
export function startAgent(record: ProjectRecord, node: AgentNode) {
  if (node.state?.status === "running" || node.state?.status === "idle") return;
  setAgentState(record, node, { status: "starting", last_error: null });
  appendLog(record, node.id, "engine", `spawning ${node.config.harness} harness`);

  later(record, 500, () => {
    if (!record.authenticated.has(node.id)) {
      setAgentState(record, node, { status: "needs_auth" });
      appendLog(record, node.id, "stderr", "no credentials for this harness — authenticate to continue");
      return;
    }
    setAgentState(record, node, { status: "idle", session_id: randomUUID(), last_activity: now() });
    appendLog(record, node.id, "engine", "system prompt applied; injected context nodes: see inspector");
    appendLog(record, node.id, "stdout", "ready");
    drain(record, node);
  });
}

export function stopAgent(record: ProjectRecord, node: AgentNode) {
  appendLog(record, node.id, "engine", "stopping harness");
  setAgentState(record, node, { status: "stopped", session_id: null });
}

export function clearContext(record: ProjectRecord, node: AgentNode) {
  appendLog(record, node.id, "engine", "context cleared; re-applying system prompt and injected ctx nodes");
  setAgentState(record, node, { status: "idle", session_id: randomUUID() });
  drain(record, node);
}

/** §3c: every message row carries its size and a hash of the body as sent. */
function messageRow(fields: { from: MessageSender; to: string; body: string }): Message {
  return {
    id: randomUUID(),
    from: fields.from,
    to: fields.to,
    body: fields.body,
    sha256: createHash("sha256").update(fields.body, "utf8").digest("hex"),
    bytes: Buffer.byteLength(fields.body, "utf8"),
    state: "queued",
    created_at: now(),
  };
}

/**
 * §3's envelope attributes are engine-generated and are the ONLY framing an agent can trust,
 * so they carry the wire name — "user", never the UI's friendlier "you".
 */
function envelopeName(from: MessageSender): string {
  return from.kind === "node" ? from.name : from.kind;
}

/** The engine names a sender by the node behind it, or by the lane it came from. */
export function senderFor(record: ProjectRecord, name: string): MessageSender {
  if (name === "user") return { kind: "user" };
  const node = record.nodes.find((n) => n.name === name);
  return node ? { kind: "node", id: node.id, name: node.name, type: node.type } : { kind: "system" };
}

export function deliver(record: ProjectRecord, to: AgentNode, fromName: string, body: string) {
  const message = messageRow({ from: senderFor(record, fromName), to: to.id, body });
  record.messages.push(message);
  emitMessage(record, message);
  drain(record, to);
  return message;
}

function emitMessage(record: ProjectRecord, message: Message) {
  emit(record, { type: "message", message: { ...message } });
}

function drain(record: ProjectRecord, node: AgentNode) {
  const status = node.state?.status;
  if (status !== "idle") return;
  // §3c #12: the user's message is ordered ahead of queued agent/endpoint/script messages.
  const pending = deliveryOrder(record.messages, node.id)[0];
  if (!pending) return;

  pending.delivered_at = now();
  pending.state = "delivered";
  emitMessage(record, pending);
  setAgentState(record, node, { status: "running", last_activity: now() });
  appendLog(
    record,
    node.id,
    "stdout",
    `<AgentPrompt id="${pending.id}" from="${envelopeName(pending.from)}" type="${senderKind(pending.from)}">`,
  );
  for (const line of pending.body.split("\n")) appendLog(record, node.id, "stdout", line);
  appendLog(record, node.id, "stdout", "</AgentPrompt>");

  later(record, 900, () => {
    appendLog(record, node.id, "stdout", `thinking about "${pending.body.slice(0, 48)}"`);
  });

  later(record, 1800, () => {
    pending.consumed_at = now();
    pending.state = "consumed";
    emitMessage(record, pending);
    const reply = messageRow({
      from: { kind: "node", id: node.id, name: node.name, type: "agent" },
      to: pending.from.kind === "node" ? pending.from.id : "user",
      body: `Read it. ${node.config.harness === "claude" ? "Claude" : "Codex"} here — ${pending.bytes} bytes, noted.`,
    });
    record.messages.push(reply);
    emitMessage(record, reply);
    appendLog(record, node.id, "stdout", reply.body);

    if (node.config.ephemeral_context) {
      appendLog(record, node.id, "engine", "ephemeral_context: clearing context after turn");
      setAgentState(record, node, { status: "idle", session_id: randomUUID(), last_activity: now() });
    } else {
      setAgentState(record, node, { status: "idle", last_activity: now() });
    }
    drain(record, node);
  });
}

export function agentStatusAfterAction(action: string): AgentStatus | null {
  return action === "stop" ? "stopped" : null;
}
