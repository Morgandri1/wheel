/**
 * Enough of §3d for the UI to be built and driven against.
 *
 * The ENGINE is the only real spec parser — web must never grow one, or the two will disagree
 * about what an operation is. This is a deliberately small stand-in that understands OpenAPI 3
 * and Swagger 2 well enough to produce the same shape the engine will, so the inspector can be
 * developed and tested now. When the engine's importer lands, this is what gets deleted.
 */
import type { Fill, ParamLocation, ToolFormat, ToolOperation, ToolParam } from "@/lib/schema";
import { EngineRefusal } from "./state";

const METHODS = ["get", "put", "post", "delete", "patch", "head", "options"] as const;

export function detectFormat(doc: Record<string, unknown>): ToolFormat {
  if (typeof doc.openapi === "string") return "openapi";
  if (typeof doc.swagger === "string") return "swagger2";
  if (doc.info && Array.isArray((doc as { item?: unknown[] }).item)) return "postman";
  if ((doc as { _type?: string })._type === "export") return "insomnia";
  return "manual";
}

/** A slug that is safe to concatenate into an MCP tool name (`<tool>__<op>`). */
function slug(method: string, path: string, operationId?: string): string {
  const base = operationId ?? `${method}_${path}`;
  return base
    .replace(/[{}]/g, "")
    .replace(/[^A-Za-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .toLowerCase()
    .slice(0, 60);
}

export function parseSpec(raw: string): { operations: ToolOperation[]; base_url: string; format: ToolFormat } {
  let doc: Record<string, unknown>;
  try {
    doc = JSON.parse(raw) as Record<string, unknown>;
  } catch {
    throw new EngineRefusal(400, "that document is not valid JSON (YAML lands with the engine)");
  }

  const format = detectFormat(doc);
  if (format === "postman" || format === "insomnia") {
    throw new EngineRefusal(400, `${format} import lands with the engine's parser`);
  }

  const base_url = baseUrlOf(doc, format);
  const paths = (doc.paths ?? {}) as Record<string, Record<string, unknown>>;
  const operations: ToolOperation[] = [];

  for (const [path, item] of Object.entries(paths)) {
    for (const method of METHODS) {
      const op = item[method] as Record<string, unknown> | undefined;
      if (!op) continue;

      const shared = (item.parameters as unknown[]) ?? [];
      const own = (op.parameters as unknown[]) ?? [];
      const params: ToolParam[] = [];
      for (const entry of [...shared, ...own] as Record<string, unknown>[]) {
        const location = entry.in as ParamLocation;
        if (!["path", "query", "header", "cookie"].includes(location)) continue;
        params.push({
          name: String(entry.name),
          location,
          description: (entry.description as string) ?? null,
          schema: (entry.schema as Record<string, unknown>) ?? { type: String(entry.type ?? "string") },
          required: Boolean(entry.required) || location === "path",
          // §3d: everything arrives as `agent`. Narrowing it is a decision the person makes.
          fill: { mode: "agent" } satisfies Fill,
        });
      }

      operations.push({
        id: slug(method, path, op.operationId as string | undefined),
        method: method.toUpperCase() as ToolOperation["method"],
        path,
        summary: (op.summary as string) ?? (op.description as string) ?? undefined,
        enabled: true,
        params,
      });
    }
  }

  if (!operations.length) throw new EngineRefusal(400, "no operations found in that document");
  return { operations, base_url, format };
}

function baseUrlOf(doc: Record<string, unknown>, format: ToolFormat): string {
  if (format === "openapi") {
    const servers = doc.servers as { url?: string }[] | undefined;
    return servers?.[0]?.url ?? "";
  }
  const host = doc.host as string | undefined;
  if (!host) return "";
  const scheme = ((doc.schemes as string[]) ?? ["https"])[0];
  return `${scheme}://${host}${(doc.basePath as string) ?? ""}`;
}

/**
 * §3d rule 5: re-import diffs by method+path and KEEPS the fills already chosen. Losing a
 * carefully configured vault reference because an upstream spec gained an endpoint would be
 * the whole feature betraying the person who set it up.
 */
export function mergeOperations(existing: ToolOperation[], incoming: ToolOperation[]) {
  const key = (o: ToolOperation) => `${o.method} ${o.path}`;
  const before = new Map(existing.map((o) => [key(o), o]));
  const after = new Map(incoming.map((o) => [key(o), o]));

  const merged = incoming.map((op) => {
    const prior = before.get(key(op));
    if (!prior) return op;
    return {
      ...op,
      enabled: prior.enabled,
      params: (op.params ?? []).map((param) => {
        const priorParam = (prior.params ?? []).find(
          (x) => x.name === param.name && x.location === param.location,
        );
        return priorParam ? { ...param, fill: priorParam.fill } : param;
      }),
    };
  });

  return {
    operations: merged,
    added: incoming.filter((o) => !before.has(key(o))).map(key),
    removed: existing.filter((o) => !after.has(key(o))).map(key),
    kept: incoming.filter((o) => before.has(key(o))).map(key),
  };
}

/**
 * §3d rule 1: only `agent`-mode fields are ever shown to an agent. Anything static, vault-backed
 * or hidden is absent from the schema, so the agent cannot supply it and cannot learn it exists.
 */
export function agentInputSchema(op: ToolOperation) {
  const properties: Record<string, unknown> = {};
  const required: string[] = [];

  for (const param of op.params ?? []) {
    if ((param.fill?.mode ?? "agent") !== "agent") continue;
    properties[param.name] = param.schema;
    if (param.required) required.push(param.name);
  }

  return { type: "object", properties, required, additionalProperties: false };
}
