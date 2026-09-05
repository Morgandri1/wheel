"use client";

/**
 * The only way web/ talks to Wheel. Never call the engine directly — everything goes through
 * api.wheel.dev, which authenticates the Clerk session and proxies to the project's container.
 *
 * Every project-scoped request carries x-auth-token and x-project-id. The token never appears in
 * a URL, a query string, or a log line.
 */
import { ApiError, getAuthToken } from "@/lib/auth";
import type { LogStreamName } from "@/lib/schema";
import type {
  AuthBegin,
  AuthStatus,
  Board,
  LogLine,
  Message,
  NodeType,
  Position,
  Project,
  ToolFormat,
  ToolOperation,
  WheelNode,
  WireType,
} from "@/lib/schema";

export const API_URL = (process.env.NEXT_PUBLIC_API_URL ?? "http://localhost:8787").replace(/\/$/, "");

interface RequestOptions {
  method?: string;
  body?: unknown;
  /** Raw bytes instead of JSON — used only for chest blob uploads. */
  raw?: BodyInit;
  projectId?: string;
  signal?: AbortSignal;
  /** Set for endpoints that answer with bytes rather than JSON. */
  expect?: "json" | "blob" | "void";
}

async function request<T>(path: string, opts: RequestOptions = {}): Promise<T> {
  const token = await getAuthToken();
  const headers: Record<string, string> = { "x-auth-token": token };
  if (opts.projectId) headers["x-project-id"] = opts.projectId;
  if (opts.body !== undefined) headers["content-type"] = "application/json";

  let res: Response;
  try {
    res = await fetch(`${API_URL}${path}`, {
      method: opts.method ?? "GET",
      headers,
      body: opts.raw ?? (opts.body !== undefined ? JSON.stringify(opts.body) : undefined),
      signal: opts.signal,
    });
  } catch (cause) {
    if ((cause as Error)?.name === "AbortError") throw cause;
    throw new ApiError(0, "offline", "Can't reach the API. Check that it's running.");
  }

  if (!res.ok) throw await toApiError(res);
  if (opts.expect === "void" || res.status === 204) return undefined as T;
  if (opts.expect === "blob") return (await res.blob()) as T;
  return (await res.json()) as T;
}

async function toApiError(res: Response): Promise<ApiError> {
  let code = `http_${res.status}`;
  let message = defaultMessage(res.status);
  try {
    const body = (await res.json()) as { error?: { code?: string; message?: string } };
    if (body?.error?.message) message = body.error.message;
    if (body?.error?.code) code = body.error.code;
  } catch {
    /* keep the default */
  }
  return new ApiError(res.status, code, message);
}

function defaultMessage(status: number): string {
  if (status === 401) return "Your session expired. Sign in again.";
  if (status === 403) return "That isn't allowed on this project.";
  if (status === 404) return "That's gone, or was never yours.";
  if (status === 409) return "That conflicts with something already there.";
  if (status >= 500) return "The API failed. Try again in a moment.";
  return "That request was rejected.";
}

// ---------------------------------------------------------------- projects (§5)

export const projects = {
  list: () => request<Project[]>("/v1/projects"),
  get: (id: string) => request<Project>(`/v1/projects/${id}`, { projectId: id }),
  create: (name: string) => request<Project>("/v1/projects", { method: "POST", body: { name } }),
  patch: (id: string, patch: { name?: string; capabilities?: { http: boolean } }) =>
    request<Project>(`/v1/projects/${id}`, { method: "PATCH", body: patch, projectId: id }),
  remove: (id: string) =>
    request<void>(`/v1/projects/${id}`, { method: "DELETE", projectId: id, expect: "void" }),
  start: (id: string) => request<Project>(`/v1/projects/${id}/start`, { method: "POST", projectId: id }),
  stop: (id: string) => request<Project>(`/v1/projects/${id}/stop`, { method: "POST", projectId: id }),
  restart: (id: string) =>
    request<Project>(`/v1/projects/${id}/restart`, { method: "POST", projectId: id }),

  /**
   * §5: a browser cannot set headers on a WebSocket handshake, and the session JWT must never
   * ride in a URL. The API mints a single-use ticket bound to (user, project) instead; it is
   * the only credential that ever appears in a query string, and it expires in 30 seconds.
   */
  wsTicket: (id: string) =>
    request<{ ticket: string; expires_in: number }>(`/v1/projects/${id}/ws-ticket`, {
      method: "POST",
      projectId: id,
    }),
};

// ---------------------------------------------------------------- engine, via the API proxy (§4)

const engine = (projectId: string, path: string) => `/v1/projects/${projectId}/engine/v1${path}`;

export function engineApi(projectId: string) {
  const p = { projectId };

  return {
    board: () => request<Board>(engine(projectId, "/board"), p),

    createNode: (input: { name: string; type: NodeType; position: Position; config?: unknown }) =>
      request<WheelNode>(engine(projectId, "/nodes"), { ...p, method: "POST", body: input }),

    patchNode: (nodeId: string, patch: { name?: string; position?: Position; config?: unknown }) =>
      request<WheelNode>(engine(projectId, `/nodes/${nodeId}`), { ...p, method: "PATCH", body: patch }),

    deleteNode: (nodeId: string) =>
      request<void>(engine(projectId, `/nodes/${nodeId}`), { ...p, method: "DELETE", expect: "void" }),

    createWire: (from: string, to: string, type: WireType) =>
      request<{ from: string; to: string; type: WireType }>(engine(projectId, "/wires"), {
        ...p,
        method: "POST",
        body: { from, to, type },
      }),

    deleteWire: (from: string, to: string, type: WireType) =>
      request<void>(engine(projectId, "/wires"), {
        ...p,
        method: "DELETE",
        body: { from, to, type },
        expect: "void",
      }),

    agent: (nodeId: string) => ({
      start: () => request<void>(engine(projectId, `/agents/${nodeId}/start`), { ...p, method: "POST" }),
      stop: () => request<void>(engine(projectId, `/agents/${nodeId}/stop`), { ...p, method: "POST" }),
      restart: () => request<void>(engine(projectId, `/agents/${nodeId}/restart`), { ...p, method: "POST" }),
      clear: () => request<void>(engine(projectId, `/agents/${nodeId}/clear`), { ...p, method: "POST" }),
      send: (body: string) =>
        request<Message>(engine(projectId, `/agents/${nodeId}/send`), { ...p, method: "POST", body: { body } }),
      /**
       * Backfill. `seq` is monotonic per agent and is the resume cursor: the socket has no
       * replay, so on reconnect you ask for everything after the last seq you saw.
       */
      log: (opts: { since?: number; stream?: LogStreamName } = {}) => {
        const q = new URLSearchParams();
        if (opts.since !== undefined) q.set("since", String(opts.since));
        if (opts.stream) q.set("stream", opts.stream);
        const query = q.toString();
        return request<{ lines: LogLine[] }>(
          engine(projectId, `/agents/${nodeId}/log${query ? `?${query}` : ""}`),
          p,
        );
      },
      authStatus: () => request<AuthStatus>(engine(projectId, `/agents/${nodeId}/auth`), p),
      authBegin: () =>
        request<AuthBegin>(engine(projectId, `/agents/${nodeId}/auth/begin`), { ...p, method: "POST" }),
      authComplete: (body: { code?: string; api_key?: string }) =>
        request<AuthStatus>(engine(projectId, `/agents/${nodeId}/auth/complete`), { ...p, method: "POST", body }),
    }),

    messages: () => request<{ messages: Message[] }>(engine(projectId, "/messages"), p),

    table: (nodeId: string) => ({
      rows: (limit = 50, offset = 0) =>
        request<{ rows: Record<string, unknown>[]; total: number }>(
          engine(projectId, `/tables/${nodeId}/rows?limit=${limit}&offset=${offset}`),
          p,
        ),
      query: (sql: string) =>
        // PROTOCOL.md: a SQL result is {columns, rows}, rows being positional arrays.
        request<QueryResult>(engine(projectId, `/tables/${nodeId}/query`), {
          ...p,
          method: "POST",
          body: { sql },
        }),
    }),

    chest: (nodeId: string) => ({
      ls: (prefix = "") =>
        request<{ entries: { key: string; bytes: number; modified_at: string }[] }>(
          engine(projectId, `/chests/${nodeId}/ls?prefix=${encodeURIComponent(prefix)}`),
          p,
        ),
      get: (key: string) =>
        request<Blob>(engine(projectId, `/chests/${nodeId}/blob?key=${encodeURIComponent(key)}`), {
          ...p,
          expect: "blob",
        }),
      put: (key: string, body: BodyInit) =>
        request<void>(engine(projectId, `/chests/${nodeId}/blob?key=${encodeURIComponent(key)}`), {
          ...p,
          method: "PUT",
          raw: body,
          expect: "void",
        }),
      remove: (key: string) =>
        request<void>(engine(projectId, `/chests/${nodeId}/blob?key=${encodeURIComponent(key)}`), {
          ...p,
          method: "DELETE",
          expect: "void",
        }),
    }),

    /** §3d. The engine is the only spec parser — web never re-implements one. */
    tools: {
      /** Normalized preview for a document that has not been saved to a node yet. */
      preview: (raw: string, format?: ToolFormat) =>
        request<{ operations: ToolOperation[]; base_url?: string; format: ToolFormat }>(
          engine(projectId, "/tools/import"),
          { ...p, method: "POST", body: { raw, format } },
        ),

      /** Re-import into an existing node: diffs by method+path and keeps the fills already set. */
      reimport: (nodeId: string, raw: string, format?: ToolFormat) =>
        request<{ operations: ToolOperation[]; added: string[]; removed: string[]; kept: string[] }>(
          engine(projectId, `/tools/${nodeId}/import`),
          { ...p, method: "POST", body: { raw, format } },
        ),

      /** Exactly what an agent would see: enabled ops, agent-mode fields only. */
      ops: (nodeId: string) =>
        request<{ operations: { id: string; description?: string; input_schema: unknown }[] }>(
          engine(projectId, `/tools/${nodeId}/ops`),
          p,
        ),

      /** Run an operation as the user. dry_run returns the equivalent curl instead of sending. */
      call: (nodeId: string, op: string, args: Record<string, unknown>, dryRun = false) =>
        request<{
          status?: number;
          headers?: Record<string, string>;
          body?: unknown;
          curl?: string;
        }>(engine(projectId, `/tools/${nodeId}/call`), {
          ...p,
          method: "POST",
          body: { op, args, dry_run: dryRun },
        }),
    },

    /**
     * Vault values are write-only. There is deliberately no getter here — not a missing feature,
     * a guarantee: nothing downstream can render or cache a secret it has no way to fetch.
     */
    putSecret: (nodeId: string, key: string, value: string) =>
      request<void>(engine(projectId, `/vault/${nodeId}/${encodeURIComponent(key)}`), {
        ...p,
        method: "PUT",
        body: { value },
        expect: "void",
      }),
  };
}

export interface QueryResult {
  columns: string[];
  rows: unknown[][];
}

export type EngineApi = ReturnType<typeof engineApi>;
export { ApiError };
