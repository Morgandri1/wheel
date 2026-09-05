/**
 * Mock api.wheel.dev. Implements the §5 public surface and proxies §4 engine routes against the
 * in-memory state in ./state.ts. Real HTTP + real WebSocket, so the browser and QA's Playwright
 * exercise the same thing the real API will serve.
 *
 *   pnpm mock          # http://localhost:8787
 */
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { WebSocketServer } from "ws";
import * as S from "./state";
import { HttpError } from "./state";
import type { WireType } from "../src/lib/schema";

const PORT = Number(process.env.MOCK_PORT ?? 8787);
const ORIGIN = process.env.MOCK_CORS_ORIGIN ?? "http://localhost:3000";

S.init();

function cors(res: ServerResponse) {
  res.setHeader("access-control-allow-origin", ORIGIN);
  res.setHeader("access-control-allow-methods", "GET,POST,PATCH,PUT,DELETE,OPTIONS");
  res.setHeader("access-control-allow-headers", "content-type,x-auth-token,x-project-id");
  res.setHeader("access-control-max-age", "86400");
}

function send(res: ServerResponse, status: number, body: unknown) {
  cors(res);
  if (body === undefined) return res.writeHead(status).end();
  const payload = JSON.stringify(body);
  res.writeHead(status, { "content-type": "application/json" }).end(payload);
}

function fail(res: ServerResponse, e: unknown) {
  if (e instanceof HttpError) return send(res, e.status, { error: { code: e.code, message: e.message } });
  console.error(e);
  return send(res, 500, { error: { code: "internal", message: "Mock server blew up. See its console." } });
}

async function readBody(req: IncomingMessage): Promise<Buffer> {
  const chunks: Buffer[] = [];
  for await (const c of req) chunks.push(c as Buffer);
  return Buffer.concat(chunks);
}

async function readJson<T>(req: IncomingMessage): Promise<T> {
  const raw = (await readBody(req)).toString("utf8");
  if (!raw) return {} as T;
  try {
    return JSON.parse(raw) as T;
  } catch {
    throw new HttpError(400, "bad_json", "That request body isn't JSON.");
  }
}

/** §5: verify token first, then project ownership. Missing token is 401, wrong project is 404. */
function requireAuth(req: IncomingMessage) {
  const token = req.headers["x-auth-token"];
  if (!token || typeof token !== "string") {
    throw new HttpError(401, "unauthenticated", "Sign in to continue.");
  }
}

function requireProjectHeader(req: IncomingMessage, id: string) {
  const header = req.headers["x-project-id"];
  if (header !== id) {
    throw new HttpError(404, "not_found", "No such project.");
  }
}

const server = createServer(async (req, res) => {
  try {
    if (req.method === "OPTIONS") {
      cors(res);
      return res.writeHead(204).end();
    }
    const url = new URL(req.url ?? "/", `http://localhost:${PORT}`);
    const path = url.pathname;
    const method = req.method ?? "GET";

    if (path === "/healthz") return send(res, 200, { ok: true });

    if (url.searchParams.has("chaos")) S.setChaosWire(url.searchParams.get("chaos") === "wire");

    if (path === "/v1/projects" && method === "GET") {
      requireAuth(req);
      return send(res, 200, S.listProjects());
    }
    if (path === "/v1/projects" && method === "POST") {
      requireAuth(req);
      const body = await readJson<{ name: string }>(req);
      return send(res, 201, S.createProject(body.name));
    }

    const projectMatch = /^\/v1\/projects\/([^/]+)(\/.*)?$/.exec(path);
    if (projectMatch) {
      requireAuth(req);
      const id = projectMatch[1]!;
      const rest = projectMatch[2] ?? "";
      requireProjectHeader(req, id);

      if (rest === "") {
        if (method === "GET") {
          const p = S.getState(id).project;
          return send(res, 200, { ...p, ingress_base_url: `http://localhost:${PORT}` });
        }
        if (method === "PATCH") return send(res, 200, S.patchProject(id, await readJson(req)));
        if (method === "DELETE") {
          S.deleteProject(id);
          return send(res, 204, undefined);
        }
      }
      if (rest === "/start" && method === "POST") {
        S.setProjectStatus(id, "starting");
        S.setProjectStatus(id, "running", 1200);
        return send(res, 202, S.getState(id).project);
      }
      if (rest === "/stop" && method === "POST") {
        S.setProjectStatus(id, "stopped");
        return send(res, 202, S.getState(id).project);
      }
      if (rest === "/restart" && method === "POST") {
        S.setProjectStatus(id, "starting");
        S.setProjectStatus(id, "running", 1200);
        return send(res, 202, S.getState(id).project);
      }

      if (rest.startsWith("/engine/")) {
        return await engine(req, res, id, rest.slice("/engine".length), url);
      }
    }

    return send(res, 404, { error: { code: "not_found", message: "No such route." } });
  } catch (e) {
    return fail(res, e);
  }
});

async function engine(
  req: IncomingMessage,
  res: ServerResponse,
  id: string,
  path: string,
  url: URL,
) {
  const method = req.method ?? "GET";
  const m = (re: RegExp) => re.exec(path);

  if (path === "/v1/board" && method === "GET") return send(res, 200, S.board(id));

  if (path === "/v1/nodes" && method === "POST") {
    return send(res, 201, S.createNode(id, await readJson(req)));
  }
  let hit = m(/^\/v1\/nodes\/([^/]+)$/);
  if (hit) {
    const nodeId = hit[1]!;
    if (method === "PATCH") return send(res, 200, S.patchNode(id, nodeId, await readJson(req)));
    if (method === "DELETE") {
      S.deleteNode(id, nodeId);
      return send(res, 204, undefined);
    }
  }

  if (path === "/v1/wires") {
    const body = await readJson<{ from: string; to: string; type: WireType }>(req);
    if (method === "POST") return send(res, 201, S.createWire(id, body.from, body.to, body.type));
    if (method === "DELETE") {
      S.deleteWire(id, body.from, body.to, body.type);
      return send(res, 204, undefined);
    }
  }

  hit = m(/^\/v1\/agents\/([^/]+)\/(start|stop|restart|clear)$/);
  if (hit && method === "POST") {
    const [, nodeId, action] = hit as unknown as [string, string, string];
    ({ start: S.startAgent, stop: S.stopAgent, restart: S.restartAgent, clear: S.clearAgent })[
      action as "start"
    ](id, nodeId);
    return send(res, 202, { ok: true });
  }

  hit = m(/^\/v1\/agents\/([^/]+)\/send$/);
  if (hit && method === "POST") {
    const body = await readJson<{ body: string }>(req);
    return send(res, 202, S.sendToAgent(id, hit[1]!, body.body));
  }

  hit = m(/^\/v1\/agents\/([^/]+)\/log$/);
  if (hit && method === "GET") {
    return send(res, 200, { lines: S.getLog(id, hit[1]!, url.searchParams.get("since") ?? undefined) });
  }

  hit = m(/^\/v1\/agents\/([^/]+)\/auth$/);
  if (hit && method === "GET") return send(res, 200, S.authStatus(id, hit[1]!));

  hit = m(/^\/v1\/agents\/([^/]+)\/auth\/begin$/);
  if (hit && method === "POST") return send(res, 200, S.authBegin(id, hit[1]!));

  hit = m(/^\/v1\/agents\/([^/]+)\/auth\/complete$/);
  if (hit && method === "POST") {
    return send(res, 200, S.authComplete(id, hit[1]!, await readJson(req)));
  }

  hit = m(/^\/v1\/messages$/);
  if (hit && method === "GET") return send(res, 200, { messages: S.messages(id) });

  hit = m(/^\/v1\/vault\/([^/]+)\/([^/]+)$/);
  if (hit && method === "PUT") {
    const body = await readJson<{ value: string }>(req);
    S.vaultPut(id, hit[1]!, decodeURIComponent(hit[2]!), body.value);
    return send(res, 204, undefined);
  }

  hit = m(/^\/v1\/tables\/([^/]+)\/rows$/);
  if (hit && method === "GET") {
    return send(
      res,
      200,
      S.tableRows(id, hit[1]!, Number(url.searchParams.get("limit") ?? 50), Number(url.searchParams.get("offset") ?? 0)),
    );
  }
  hit = m(/^\/v1\/tables\/([^/]+)\/query$/);
  if (hit && method === "POST") {
    const body = await readJson<{ sql: string }>(req);
    return send(res, 200, S.tableQuery(id, hit[1]!, body.sql));
  }
  hit = m(/^\/v1\/tables\/([^/]+)\/rows$/);
  if (hit && method === "POST") {
    return send(res, 201, S.tableInsert(id, hit[1]!, await readJson(req)));
  }

  hit = m(/^\/v1\/chests\/([^/]+)\/ls$/);
  if (hit && method === "GET") {
    return send(res, 200, S.chestLs(id, hit[1]!, url.searchParams.get("prefix") ?? ""));
  }
  hit = m(/^\/v1\/chests\/([^/]+)\/blob$/);
  if (hit) {
    const nodeId = hit[1]!;
    const key = url.searchParams.get("key") ?? "";
    if (method === "GET") {
      const buf = S.chestGet(id, nodeId, key);
      cors(res);
      return res.writeHead(200, { "content-type": "application/octet-stream" }).end(buf);
    }
    if (method === "PUT") {
      S.chestPut(id, nodeId, key, await readBody(req));
      return send(res, 204, undefined);
    }
    if (method === "DELETE") {
      S.chestDelete(id, nodeId, key);
      return send(res, 204, undefined);
    }
  }

  return send(res, 404, { error: { code: "not_found", message: `No engine route ${method} ${path}.` } });
}

// ---------------------------------------------------------------- events websocket

const wss = new WebSocketServer({ noServer: true });

server.on("upgrade", (req, socket, head) => {
  const url = new URL(req.url ?? "/", `http://localhost:${PORT}`);
  const hit = /^\/v1\/projects\/([^/]+)\/engine\/v1\/events$/.exec(url.pathname);
  if (!hit) {
    socket.destroy();
    return;
  }
  // The Clerk token never goes in a URL, so the browser passes it as a subprotocol.
  const id = hit[1]!;
  wss.handleUpgrade(req, socket, head, (ws) => {
    let state;
    try {
      state = S.getState(id);
    } catch {
      ws.close(4404, "no such project");
      return;
    }
    ws.send(JSON.stringify({ type: "board.changed", project_id: id, ts: new Date().toISOString() }));
    void state;
    const off = S.subscribe(id, (e) => {
      if (ws.readyState === ws.OPEN) ws.send(JSON.stringify(e));
    });
    const ping = setInterval(() => ws.readyState === ws.OPEN && ws.ping(), 20_000);
    ws.on("close", () => {
      off();
      clearInterval(ping);
    });
  });
});

server.listen(PORT, () => {
  console.log(`mock api.wheel.dev on http://localhost:${PORT}  (CORS origin ${ORIGIN})`);
  console.log(`seed project: ${S.listProjects()[0]?.id}`);
});
