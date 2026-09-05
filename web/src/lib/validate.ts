import { NODE_NAME_RE, type NodeType, type WheelNode } from "@/lib/schema";

export interface Invalid {
  ok: false;
  reason: string;
}
export type Valid = { ok: true };
export type Check = Valid | Invalid;

const ok: Valid = { ok: true };

/**
 * §3: a node's name is its address — the string other agents type into
 * `wheel msg`. Same rule the engine applies, so people find out here first.
 */
export function checkNodeName(name: string, taken: readonly string[] = []): Check {
  if (name.length === 0) return { ok: false, reason: "Give the node a name — agents address it by this." };
  if (name.length > 63) return { ok: false, reason: "Names stop at 63 characters." };
  if (!NODE_NAME_RE.test(name)) {
    if (/[A-Z]/.test(name)) return { ok: false, reason: "Lower case only." };
    if (/^[-_]/.test(name)) return { ok: false, reason: "Start with a letter or a digit." };
    return { ok: false, reason: "Use letters, digits, hyphens and underscores." };
  }
  if (taken.includes(name)) return { ok: false, reason: `There is already a node called ${name}.` };
  return ok;
}

/** §3 endpoint config: leading slash, no `..`. */
export function checkEndpointPath(path: string): Check {
  if (!path.startsWith("/")) return { ok: false, reason: "Start the path with a slash." };
  if (path.includes("..")) return { ok: false, reason: "`..` is not allowed in a path." };
  if (/\s/.test(path)) return { ok: false, reason: "Paths cannot contain spaces." };
  return ok;
}

/** §3 chest keys: relative paths, no `..`, no absolute paths. */
export function checkChestKey(key: string): Check {
  if (key.length === 0) return { ok: false, reason: "Give the file a path." };
  if (key.startsWith("/")) return { ok: false, reason: "Chest paths are relative — drop the leading slash." };
  if (key.split("/").includes("..")) return { ok: false, reason: "`..` is not allowed in a path." };
  return ok;
}

export const CHEST_MAX_BLOB_BYTES = 50 * 1024 * 1024;

export function checkBlobSize(bytes: number): Check {
  if (bytes > CHEST_MAX_BLOB_BYTES) {
    return { ok: false, reason: `Files stop at 50 MiB. That one is ${(bytes / 1024 / 1024).toFixed(1)} MiB.` };
  }
  return ok;
}

/** Table column names become sqlite columns; `key` is implicit (§3, v1.1). */
export function checkColumnName(name: string, existing: readonly string[] = []): Check {
  if (name === "key") return { ok: false, reason: "Every table already has a `key` column." };
  if (!/^[a-z_][a-z0-9_]*$/.test(name)) {
    return { ok: false, reason: "Lower case letters, digits and underscores; start with a letter." };
  }
  if (existing.includes(name)) return { ok: false, reason: `The table already has a ${name} column.` };
  return ok;
}

/** Read-only SQL box: block anything that is not a single SELECT. */
export function checkReadOnlySql(sql: string): Check {
  const trimmed = sql.trim().replace(/;\s*$/, "");
  if (trimmed.length === 0) return { ok: false, reason: "Write a SELECT." };
  if (trimmed.includes(";")) return { ok: false, reason: "One statement at a time." };
  if (!/^select\b/i.test(trimmed)) return { ok: false, reason: "Only SELECT runs here." };
  return ok;
}

/** Names already on the board, excluding one node (so renaming to itself is fine). */
export function takenNames(nodes: readonly WheelNode[], exceptId?: string): string[] {
  return nodes.filter((n) => n.id !== exceptId).map((n) => n.name);
}

/** A fresh, unused name for a newly placed node: `agent`, `agent-2`, `agent-3`… */
export function suggestName(type: NodeType, taken: readonly string[]): string {
  if (!taken.includes(type)) return type;
  for (let i = 2; i < 1000; i++) {
    const candidate = `${type}-${i}`;
    if (!taken.includes(candidate)) return candidate;
  }
  return `${type}-${Date.now()}`;
}
