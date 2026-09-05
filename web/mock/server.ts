/**
 * Mock api.wheel.dev — §5 routes, proxying to a §4-shaped in-memory engine.
 *
 *   pnpm mock          → http://localhost:8787
 *   NEXT_PUBLIC_API_URL=http://localhost:8787 pnpm dev
 *
 * It enforces the §3 wire matrix independently of the browser, refuses
 * unauthenticated calls, and 404s projects it does not own — so the failure
 * paths in the UI are exercised in development, not discovered in production.
 */
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { randomUUID } from "node:crypto";
import { TicketStore } from "./tickets";
import { WebSocketServer, type WebSocket } from "ws";
import type { AgentNode, EngineEvent, ToolOperation, ToolParam, WheelNode, WireType } from "@/lib/schema";
import {
  EngineRefusal,
  OWNER,
  appendLog,
  assertWireLegal,
  boardChanged,
  clearContext,
  createProject,
  deliver,
  findNode,
  makeNode,
  now,
  projects,
  startAgent,
  setAgentState,
  stopAgent,
  type ProjectRecord,
} from "./state";
import { seed } from "./fixtures";
import { assertMatrixMatchesEngine } from "./assert-matrix";
import { agentInputSchema, mergeOperations, parseSpec } from "./tools";
import * as localAuth from "./local-auth";

type Credentials = { email?: unknown; password?: unknown };

const PORT = Number(process.env.MOCK_PORT ?? 8787);
// The dev server is usually on 3000; MOCK_ORIGINS lets a second one (a different auth mode, say)
// be allowed too. A browser cannot tell a CORS refusal from an unreachable server, so an origin
// missing from this list reads in the UI as "Can't reach the API" — the same trap as an unset
// CORS_ALLOWED_ORIGINS in production.
const ORIGINS = (process.env.MOCK_ORIGINS ?? "http://localhost:3000,http://127.0.0.1:3000")
  .split(",")
  .map((o) => o.trim())
  .filter(Boolean);

seed();
localAuth.seedUser();

// ── plumbing ────────────────────────────────────────────────────────────────

function cors(req: IncomingMessage, res: ServerResponse) {
  const origin = req.headers.origin;
  res.setHeader("access-control-allow-origin", origin && ORIGINS.includes(origin) ? origin : ORIGINS[0]!);
  res.setHeader("access-control-allow-methods", "GET,POST,PATCH,PUT,DELETE,OPTIONS");
  res.setHeader("access-control-allow-headers", "content-type,x-auth-token,x-project-id");
  res.setHeader("access-control-expose-headers", "retry-after");
  res.setHeader("access-control-max-age", "600");
}

function json(res: ServerResponse, status: number, body: unknown, headers: Record<string, string> = {}) {
  const payload = JSON.stringify(body);
  res.writeHead(status, { "content-type": "application/json", ...headers });
  res.end(payload);
}

const noContent = (res: ServerResponse) => {
  res.writeHead(204);
  res.end();
};

async function readBody(req: IncomingMessage): Promise<Buffer> {
  const chunks: Buffer[] = [];
  for await (const chunk of req) chunks.push(chunk as Buffer);
  return Buffer.concat(chunks);
}

async function readJson<T>(req: IncomingMessage): Promise<T> {
  const raw = await readBody(req);
  if (raw.length === 0) return {} as T;
  try {
    return JSON.parse(raw.toString("utf8")) as T;
  } catch {
    throw new EngineRefusal(400, "body is not valid json");
  }
}

/** §5: verify the token, then load, then assert ownership. Never in another order. */
function requireAuth(req: IncomingMessage) {
  const token = req.headers["x-auth-token"];
  if (typeof token !== "string" || token.length === 0) {
    throw new EngineRefusal(401, "missing x-auth-token", "unauthenticated");
  }
  // A local-mode token is a real session here, so signing out actually invalidates it and the
  // client's "any 401 means signed out" path can be exercised. Mock and dev tokens are opaque
  // strings the mock has no opinion about, and every project belongs to OWNER either way.
  if (localAuth.isLocalToken(token) && !localAuth.userForToken(token)) {
    throw new EngineRefusal(401, "that session is no longer valid", "unauthenticated");
  }
  return OWNER;
}

function requireProject(req: IncomingMessage, id: string): ProjectRecord {
  const owner = requireAuth(req);
  const record = projects.get(id);
  // A project you do not own is indistinguishable from one that does not exist.
  if (!record || record.project.owner_id !== owner) {
    throw new EngineRefusal(404, "project not found");
  }
  return record;
}

const agentOf = (record: ProjectRecord, id: string): AgentNode => {
  const node = findNode(record, id);
  if (!node) throw new EngineRefusal(404, "node not found");
  if (node.type !== "agent") throw new EngineRefusal(400, `${node.name} is a ${node.type}, not an agent`);
  return node;
};

// ── engine routes (§4), reached through /v1/projects/:id/engine/* ───────────

async function engine(
  record: ProjectRecord,
  method: string,
  path: string,
  url: URL,
  req: IncomingMessage,
  res: ServerResponse,
): Promise<boolean> {
  const project = record.project;

  if (method === "GET" && path === "/v1/board") {
    json(res, 200, { nodes: record.nodes, project });
    return true;
  }

  if (method === "POST" && path === "/v1/nodes") {
    const body = await readJson<{ name: string; type: WheelNode["type"]; position: { x: number; y: number }; config: unknown }>(req);
    if (record.nodes.some((n) => n.name === body.name)) {
      throw new EngineRefusal(409, `a node called ${body.name} already exists`);
    }
    // The engine requires `config` and answers 422 without it. The mock must be at least as
    // strict: a lenient mock lets the board work here and 422 against the real engine, which is
    // exactly how this was missed until the first real-API pass.
    if (body.config === undefined || body.config === null) {
      throw new EngineRefusal(422, "missing field `config`");
    }
    const node = makeNode(body.type, body.name, body.position, body.config);
    record.nodes.push(node);
    boardChanged(record);
    json(res, 201, node);
    return true;
  }

  const nodeMatch = /^\/v1\/nodes\/([^/]+)$/.exec(path);
  if (nodeMatch) {
    const node = findNode(record, nodeMatch[1]!);
    if (!node) throw new EngineRefusal(404, "node not found");

    if (method === "PATCH") {
      const patch = await readJson<Partial<Pick<WheelNode, "name" | "position" | "config">>>(req);
      if (patch.name && patch.name !== node.name) {
        // §4: a running agent's name is embedded in every peer's preamble and in its own session.
        if (node.type === "agent" && (node.state?.status === "running" || node.state?.status === "starting")) {
          throw new EngineRefusal(409, "agent_running: stop or park the agent before renaming it");
        }
        if (record.nodes.some((n) => n.name === patch.name && n.id !== node.id)) {
          throw new EngineRefusal(409, `a node called ${patch.name} already exists`);
        }
        node.name = patch.name;
      }
      if (patch.position) node.position = patch.position;
      if (patch.config) node.config = { ...(node.config as object), ...(patch.config as object) } as WheelNode["config"];
      boardChanged(record);
      json(res, 200, node);
      return true;
    }

    if (method === "DELETE") {
      record.nodes = record.nodes.filter((n) => n.id !== node.id);
      for (const other of record.nodes) other.wires = other.wires!.filter((w) => w.to !== node.id);
      record.tables.delete(node.id);
      record.chests.delete(node.id);
      boardChanged(record);
      noContent(res);
      return true;
    }
  }

  if (path === "/v1/wires" && (method === "POST" || method === "DELETE")) {
    const body = await readJson<{ from: string; to: string; type: WireType }>(req);
    const from = findNode(record, body.from);
    const to = findNode(record, body.to);
    if (!from || !to) throw new EngineRefusal(404, "node not found");

    if (method === "POST") {
      assertWireLegal(from, to, body.type);
      if (from.wires!.some((w) => w.to === to.id && w.type === body.type)) {
        throw new EngineRefusal(409, "that wire already exists");
      }
      from.wires!.push({ to: to.id, type: body.type });
    } else {
      from.wires! = from.wires!.filter((w) => !(w.to === to.id && w.type === body.type));
    }
    boardChanged(record);
    noContent(res);
    return true;
  }

  const agentMatch = /^\/v1\/agents\/([^/]+)\/(start|stop|restart|clear|send|log|messages)$/.exec(path);
  if (agentMatch) {
    const node = agentOf(record, agentMatch[1]!);
    const action = agentMatch[2]!;

    if (method === "POST") {
      if (action === "start") startAgent(record, node);
      else if (action === "stop") stopAgent(record, node);
      else if (action === "restart") {
        stopAgent(record, node);
        setTimeout(() => startAgent(record, node), 250);
      } else if (action === "clear") clearContext(record, node);
      else if (action === "send") {
        const body = await readJson<{ body: string }>(req);
        const message = deliver(record, node, "user", body.body);
        json(res, 202, message);
        return true;
      }
      noContent(res);
      return true;
    }

    if (method === "GET" && action === "log") {
      // `since` is the resume cursor after a reconnect (seq is monotonic per agent); `stream`
      // narrows to one voice — chiefly `transcript`, the bytes written to the child's stdin.
      const since = Number(url.searchParams.get("since") ?? 0);
      const stream = url.searchParams.get("stream");
      if (stream && !LOG_STREAMS.includes(stream)) {
        throw new EngineRefusal(400, `unknown stream ${stream} — one of ${LOG_STREAMS.join(", ")}`);
      }
      const lines = record.log.filter(
        (l) =>
          l.node_id === node.id &&
          l.seq > since &&
          (!stream || (l.stream as string) === stream),
      );
      json(res, 200, { lines });
      return true;
    }

    if (method === "GET" && action === "messages") {
      const messages = record.messages.filter((m) => m.to === node.name || m.from.kind === "node" && m.from.id === node.name);
      json(res, 200, { messages });
      return true;
    }
  }

  const authMatch = /^\/v1\/agents\/([^/]+)\/auth(\/(begin|complete))?$/.exec(path);
  if (authMatch) {
    const node = agentOf(record, authMatch[1]!);
    const step = authMatch[3];

    if (method === "POST" && step === "begin") {
      const claude = node.config.harness === "claude";
      json(res, 200, {
        mode: claude ? "device_code" : "api_key",
        url: claude ? "https://claude.ai/device" : undefined,
        user_code: claude ? "WHEL-0R81" : undefined,
        instructions: claude
          ? "Open the link, enter the code, then come back and confirm."
          : "Paste an API key for this harness. It is stored in the project, never shown again.",
      });
      return true;
    }

    if (method === "POST" && step === "complete") {
      const body = await readJson<{ code?: string; api_key?: string }>(req);
      if (!body.code && !body.api_key) throw new EngineRefusal(400, "no code or api key supplied");
      if (body.api_key && body.api_key.trim().length < 8) {
        throw new EngineRefusal(400, "that key is too short to be real");
      }
      record.authenticated.add(node.id);
      appendLog(record, node.id, "engine", "credentials accepted");
      // Authenticating does NOT start a process. Queued messages are preserved and delivered
      // after the agent is restarted, so the mock leaves it stopped and the UI has to say so —
      // auto-starting here would hide the one step the person still has to take.
      if (node.state?.status === "needs_auth") {
        setAgentState(record, node, { status: "stopped" });
      }
      json(res, 200, { authenticated: true, account: "you@example.com" });
      return true;
    }

    if (method === "GET" && !step) {
      json(res, 200, {
        authenticated: record.authenticated.has(node.id),
        account: record.authenticated.has(node.id) ? "you@example.com" : undefined,
      });
      return true;
    }
  }

  // §3d tool routes. The engine is the only real parser; ./tools is a stand-in so the inspector
  // can be built now, and is what gets deleted when the engine's importer lands.
  if (path === "/v1/tools/import" && method === "POST") {
    const { raw } = await readJson<{ raw: string; format?: string }>(req);
    json(res, 200, parseSpec(raw ?? ""));
    return true;
  }

  const toolImport = /^\/v1\/tools\/([^/]+)\/import$/.exec(path);
  if (toolImport && method === "POST") {
    const node = findNode(record, toolImport[1]!);
    if (!node || node.type !== "tool") throw new EngineRefusal(404, "tool not found");
    const { raw } = await readJson<{ raw: string }>(req);
    const parsed = parseSpec(raw ?? "");
    const diff = mergeOperations(node.config.operations ?? [], parsed.operations);
    node.config = { ...node.config, base_url: parsed.base_url, operations: diff.operations };
    boardChanged(record);
    json(res, 200, diff);
    return true;
  }

  const toolOps = /^\/v1\/tools\/([^/]+)\/ops$/.exec(path);
  if (toolOps && method === "GET") {
    const node = findNode(record, toolOps[1]!);
    if (!node || node.type !== "tool") throw new EngineRefusal(404, "tool not found");
    json(res, 200, {
      operations: (node.config.operations ?? [])
        .filter((op) => op.enabled !== false)
        .map((op) => ({
          id: op.id,
          description: op.summary ?? undefined,
          input_schema: agentInputSchema(op),
        })),
    });
    return true;
  }

  const toolCall = /^\/v1\/tools\/([^/]+)\/call$/.exec(path);
  if (toolCall && method === "POST") {
    const node = findNode(record, toolCall[1]!);
    if (!node || node.type !== "tool") throw new EngineRefusal(404, "tool not found");
    const { op: opId, args = {}, dry_run } = await readJson<{
      op: string;
      args?: Record<string, unknown>;
      dry_run?: boolean;
    }>(req);
    const op = (node.config.operations ?? []).find((o) => o.id === opId);
    if (!op) throw new EngineRefusal(404, "no such operation");

    // §3d rule 1: an agent-supplied field that is not agent-mode is rejected outright, and the
    // denial is an event rather than a silent drop.
    const agentFields = new Set(
      (op.params ?? []).filter((pm) => (pm.fill?.mode ?? "agent") === "agent").map((pm) => pm.name),
    );
    const extra = Object.keys(args).filter((k) => !agentFields.has(k));
    if (extra.length) {
      throw new EngineRefusal(400, `fields not open to the caller: ${extra.join(", ")}`);
    }

    const url = renderUrl(node.config.base_url ?? "", op, args);
    if (dry_run) {
      json(res, 200, { curl: renderCurl(node.config.base_url ?? "", op, args) });
      return true;
    }
    json(res, 200, {
      status: 200,
      headers: { "content-type": "application/json" },
      body: { ok: true, note: "mock response — the engine performs the real request", url },
    });
    return true;
  }

  const vaultMatch = /^\/v1\/vault\/([^/]+)\/(.+)$/.exec(path);
  if (vaultMatch && method === "PUT") {
    const node = findNode(record, vaultMatch[1]!);
    if (!node || node.type !== "vault") throw new EngineRefusal(404, "vault not found");
    const body = await readJson<{ value: string }>(req);
    if (!body.value) throw new EngineRefusal(400, "value is required");
    const key = decodeURIComponent(vaultMatch[2]!);
    const keys = record.vaults.get(node.id) ?? new Set<string>();
    keys.add(key);
    record.vaults.set(node.id, keys);
    node.config = { keys: [...keys] };
    boardChanged(record);
    noContent(res); // The value is never echoed back. Not even here.
    return true;
  }

  const tableRows = /^\/v1\/tables\/([^/]+)\/rows$/.exec(path);
  if (tableRows && method === "GET") {
    const rows = [...(record.tables.get(tableRows[1]!) ?? new Map()).values()];
    const limit = Number(url.searchParams.get("limit") ?? 50);
    const offset = Number(url.searchParams.get("offset") ?? 0);
    json(res, 200, { rows: rows.slice(offset, offset + limit), total: rows.length });
    return true;
  }

  const tableQuery = /^\/v1\/tables\/([^/]+)\/query$/.exec(path);
  if (tableQuery && method === "POST") {
    const body = await readJson<{ sql: string }>(req);
    if (!/^\s*select\b/i.test(body.sql)) throw new EngineRefusal(400, "only SELECT is allowed here");
    const rows = [...(record.tables.get(tableQuery[1]!) ?? new Map()).values()];
    const columns = rows.length ? Object.keys(rows[0]!) : ["key"];
    json(res, 200, { columns, rows: rows.map((r) => columns.map((c) => r[c] ?? null)) });
    return true;
  }

  const chestLs = /^\/v1\/chests\/([^/]+)\/ls$/.exec(path);
  if (chestLs && method === "GET") {
    const prefix = url.searchParams.get("prefix") ?? "";
    const blobs = record.chests.get(chestLs[1]!) ?? new Map<string, Buffer>();
    json(res, 200, {
      entries: [...blobs.entries()]
        .filter(([key]) => key.startsWith(prefix))
        .map(([key, buf]) => ({ key, size: buf.length, updated_at: now() })),
    });
    return true;
  }

  const chestBlob = /^\/v1\/chests\/([^/]+)\/blob$/.exec(path);
  if (chestBlob) {
    const nodeId = chestBlob[1]!;
    const key = url.searchParams.get("key") ?? "";
    const blobs = record.chests.get(nodeId) ?? new Map<string, Buffer>();
    if (method === "PUT") {
      blobs.set(key, await readBody(req));
      record.chests.set(nodeId, blobs);
      boardChanged(record);
      noContent(res);
      return true;
    }
    if (method === "GET") {
      const blob = blobs.get(key);
      if (!blob) throw new EngineRefusal(404, "no such file");
      res.writeHead(200, { "content-type": "application/octet-stream" });
      res.end(blob);
      return true;
    }
    if (method === "DELETE") {
      blobs.delete(key);
      noContent(res);
      return true;
    }
  }

  const scriptRun = /^\/v1\/scripts\/([^/]+)\/run$/.exec(path);
  if (scriptRun && method === "POST") {
    const node = findNode(record, scriptRun[1]!);
    if (!node || node.type !== "script") throw new EngineRefusal(404, "script not found");
    const body = await readJson<{ args: string[] }>(req);
    json(res, 200, {
      stdout: `${node.config.language} ran with args: ${JSON.stringify(body.args ?? [])}\n`,
      stderr: "",
      exit_code: 0,
    });
    return true;
  }

  return false;
}

// ── public API routes (§5) ──────────────────────────────────────────────────

async function route(req: IncomingMessage, res: ServerResponse) {
  const url = new URL(req.url ?? "/", `http://localhost:${PORT}`);
  const path = url.pathname;
  const method = req.method ?? "GET";

  if (path === "/healthz") return json(res, 200, { ok: true, mock: true });

  // Local email/password auth (AUTH_MODE=local). Unauthenticated by definition, except /me.
  if (path === "/v1/auth/signup" && method === "POST") {
    return json(res, 201, localAuth.signup(await readJson<Credentials>(req)));
  }
  if (path === "/v1/auth/login" && method === "POST") {
    return json(res, 200, localAuth.login(await readJson<Credentials>(req)));
  }
  if (path === "/v1/auth/logout" && method === "POST") {
    const token = req.headers["x-auth-token"];
    if (typeof token === "string") localAuth.logout(token);
    return noContent(res);
  }
  if (path === "/v1/auth/me" && method === "GET") {
    const token = req.headers["x-auth-token"];
    if (typeof token !== "string" || !token) throw new EngineRefusal(401, "no session", "unauthenticated");
    return json(res, 200, localAuth.me(token));
  }

  if (path === "/v1/projects" && method === "GET") {
    requireAuth(req);
    return json(res, 200, [...projects.values()].map((r) => r.project));
  }

  if (path === "/v1/projects" && method === "POST") {
    requireAuth(req);
    const body = await readJson<{ name: string }>(req);
    if (!body.name?.trim()) throw new EngineRefusal(400, "name is required");
    const record = createProject(body.name.trim());
    return json(res, 201, record.project);
  }

  const projectMatch = /^\/v1\/projects\/([^/]+)(\/.*)?$/.exec(path);
  if (projectMatch) {
    const id = projectMatch[1]!;
    const rest = projectMatch[2] ?? "";
    const record = requireProject(req, id);

    if (rest === "" && method === "GET") return json(res, 200, record.project);

    if (rest === "" && method === "PATCH") {
      const patch = await readJson<{ name?: string; capabilities?: { http: boolean } }>(req);
      if (patch.name) record.project.name = patch.name;
      if (patch.capabilities) record.project.capabilities = patch.capabilities;
      record.project.updated_at = now();
      return json(res, 200, record.project);
    }

    if (rest === "" && method === "DELETE") {
      for (const timer of record.timers) clearTimeout(timer);
      projects.delete(id);
      return noContent(res);
    }

    if (rest === "/ws-ticket" && method === "POST") {
      return json(res, 200, { ticket: mintTicket(id), expires_in: 30 });
    }

    const lifecycle = /^\/(start|stop|restart)$/.exec(rest);
    if (lifecycle && method === "POST") {
      const action = lifecycle[1]!;
      if (action === "stop") {
        record.project.status = "stopped";
        for (const node of record.nodes) if (node.type === "agent") stopAgent(record, node);
      } else {
        record.project.status = "starting";
        setTimeout(() => {
          record.project.status = "running";
          for (const node of record.nodes) {
            if (node.type === "agent" && node.config.run_on_startup) startAgent(record, node);
          }
        }, 700);
      }
      record.project.updated_at = now();
      return json(res, 200, record.project);
    }

    if (rest.startsWith("/engine/")) {
      const handled = await engine(record, method, rest.slice("/engine".length), url, req, res);
      if (handled) return;
    }
  }

  // Public ingress (§5). 403 when the http capability is off.
  const ingress = /^\/p\/([^/]+)(\/.*)?$/.exec(path);
  if (ingress) {
    const record = projects.get(ingress[1]!);
    if (!record) throw new EngineRefusal(404, "not found");
    if (!record.project.capabilities.http) throw new EngineRefusal(403, "http capability is off for this project");
    const hitPath = ingress[2] ?? "/";
    const endpoint = record.nodes.find((n) => n.type === "endpoint" && n.config.path === hitPath);
    if (!endpoint) throw new EngineRefusal(404, "no endpoint node for that path");
    const body = (await readBody(req)).toString("utf8");
    for (const wire of endpoint.wires ?? []) {
      const target = findNode(record, wire.to);
      if (target?.type === "agent") {
        deliver(record, target, endpoint.name, JSON.stringify({ method, path: hitPath, body }));
      }
    }
    return json(res, 202, { accepted: true, id: randomUUID() });
  }

  throw new EngineRefusal(404, `no route for ${method} ${path}`);
}

const server = createServer((req, res) => {
  cors(req, res);
  if (req.method === "OPTIONS") {
    res.writeHead(204);
    return res.end();
  }
  route(req, res).catch((error: unknown) => {
    if (error instanceof EngineRefusal) {
      // docs/API.md: { error: { code, message } }. A bare string here would be parsed away by the
      // web client and every refusal would read as generic fallback copy.
      const body = { error: { code: error.code ?? `http_${error.status}`, message: error.message } };
      return json(res, error.status, body, error.headers ?? {});
    }
    console.error(error);
    json(res, 500, { error: "mock server blew up — see its console" });
  });
});

// ── events websocket ────────────────────────────────────────────────────────

const tickets = new TicketStore();
const mintTicket = (projectId: string) => tickets.mint(projectId, randomUUID());


const LOG_STREAMS = ["stdout", "stderr", "engine", "transcript"];

const wss = new WebSocketServer({ noServer: true });

server.on("upgrade", (req, socket, head) => {
  const url = new URL(req.url ?? "/", `http://localhost:${PORT}`);
  const match = /^\/v1\/projects\/([^/]+)\/engine\/v1\/events$/.exec(url.pathname);
  const record = match ? projects.get(match[1]!) : undefined;

  const authorised = match ? tickets.redeem(url.searchParams.get("ticket"), match[1]!) : false;

  if (!record || !authorised) {
    socket.write("HTTP/1.1 401 Unauthorized\r\n\r\n");
    return socket.destroy();
  }

  wss.handleUpgrade(req, socket, head, (ws) => attach(ws, record));
});

function attach(ws: WebSocket, record: ProjectRecord) {
  const listener = (event: EngineEvent) => {
    if (ws.readyState === ws.OPEN) ws.send(JSON.stringify(event));
  };
  record.listeners.add(listener);
  ws.send(JSON.stringify({ type: "board.changed", project_id: record.project.id, ts: now() }));
  ws.on("close", () => record.listeners.delete(listener));
  ws.on("error", () => record.listeners.delete(listener));
}

const allowedWireCount = assertMatrixMatchesEngine();

server.listen(PORT, () => {
  const [first] = [...projects.values()];
  console.log(`mock api on http://localhost:${PORT}`);
  console.log(`seeded project: ${first?.project.name} (${first?.project.id})`);
  console.log(`wire matrix agrees with the engine's export (${allowedWireCount} allowed triples)`);
});

/**
 * §3d rule 1: an operator-pinned field is never shown to the caller — a static value no less
 * than a vault one. The agent did not supply it, cannot override it, and has no business
 * learning it; a base_url query key or an API version header pinned as `static` is exactly the
 * kind of thing that leaks a tenant id or an internal path.
 *
 * ADVERSARY F012: the two renderers previously masked `vault` and printed `static` verbatim,
 * while this comment claimed both were covered. One function decides it now, so the curl and
 * the URL cannot drift apart again.
 */
function renderValue(param: ToolParam, args: Record<string, unknown>): string | null {
  const mode = param.fill?.mode ?? "agent";
  if (mode === "hidden") return null;
  if (mode === "vault" || mode === "static") return MASK;
  return String(args[param.name] ?? "");
}

const MASK = "****";

function renderUrl(baseUrl: string, op: ToolOperation, args: Record<string, unknown>): string {
  let path = op.path;
  const query: string[] = [];

  for (const param of op.params ?? []) {
    const value = renderValue(param, args);
    if (value === null) continue;
    if (param.location === "path") path = path.replace(`{${param.name}}`, encodeURIComponent(value));
    if (param.location === "query" && value) {
      query.push(`${encodeURIComponent(param.name)}=${encodeURIComponent(value)}`);
    }
  }

  return `${baseUrl.replace(/\/$/, "")}${path}${query.length ? `?${query.join("&")}` : ""}`;
}

function renderCurl(baseUrl: string, op: ToolOperation, args: Record<string, unknown>): string {
  const parts = [`curl -X ${op.method}`];
  for (const param of op.params ?? []) {
    if (param.location !== "header") continue;
    const value = renderValue(param, args);
    if (value === null) continue;
    parts.push(`-H '${param.name}: ${value}'`);
  }
  parts.push(`'${renderUrl(baseUrl, op, args)}'`);
  return parts.join(" ");
}
