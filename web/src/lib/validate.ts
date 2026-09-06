import { NODE_NAME_RE, type NodeType } from "@/lib/schema";

/**
 * A table node IS its sqlite table (`t_<name>`), and `-` is a subtraction operator in SQL — so a
 * table's name has to be an identifier already rather than be silently mangled into one.
 *
 * The engine's rule is the node-name charset MINUS `-`; a leading digit stays legal, because the
 * `t_` prefix makes `t_9lives` a perfectly good identifier. Deliberately not stricter than the
 * engine: a client that refuses a name the server accepts is a bug that looks like a rule.
 */
const TABLE_NAME_RE = /^[a-z0-9][a-z0-9_]{0,62}$/;

/** Word for word what the engine says, so the same mistake never gets two explanations. */
const TABLE_NAME_MESSAGE =
  "A table node's name becomes the sqlite table `t_<name>`, so it cannot contain “-” (use “_” instead).";

/** Names are addresses other agents send to, so the rule is strict and the message says why. */
export function validateNodeName(
  name: string,
  taken: string[] = [],
  type?: NodeType,
): string | null {
  if (!name) return "Every node needs a name — agents address each other by it.";
  if (name.length > 63) return "Names stop at 63 characters.";
  if (!NODE_NAME_RE.test(name)) {
    return "Use lowercase letters, digits, hyphen and underscore, starting with a letter or digit.";
  }
  // Checked after the general rule so the more specific message is the one that survives.
  if (type === "table" && !TABLE_NAME_RE.test(name)) return TABLE_NAME_MESSAGE;
  if (taken.includes(name)) return `Another node is already called “${name}”.`;
  return null;
}

export function validateEndpointPath(path: string): string | null {
  if (!path.startsWith("/")) return "Paths start with a slash.";
  if (path.includes("..")) return "Paths can't contain “..”.";
  if (/\s/.test(path)) return "Paths can't contain spaces.";
  return null;
}

/**
 * Matches wheel-core's `Ident`: starts with a lowercase letter OR DIGIT, then letters, digits and
 * underscore. This previously allowed a leading `_` the engine rejects and refused a leading digit
 * the engine accepts — wrong in both directions, and each way produces a confusing round trip.
 */
export function validateColumnName(name: string): string | null {
  if (!/^[a-z0-9][a-z0-9_]{0,62}$/.test(name)) {
    return "Column names use lowercase letters, digits and underscore, starting with a letter or digit.";
  }
  return null;
}

export function validateChestKey(key: string): string | null {
  if (!key) return "Give the file a name.";
  if (key.startsWith("/")) return "Keys are relative — drop the leading slash.";
  if (key.includes("..")) return "Keys can't contain “..”.";
  return null;
}

/**
 * A default name that is free on this board: agent, agent-2, agent-3…
 *
 * Tables get `table_2`, because `table-2` is a name the engine refuses — placing a second table
 * node used to suggest a name that could not be created, which reads as the board being broken
 * rather than the suggestion being wrong.
 */
export function suggestName(type: NodeType, taken: string[]): string {
  const base = type;
  const sep = type === "table" ? "_" : "-";
  if (!taken.includes(base)) return base;
  for (let i = 2; i < 500; i++) {
    const candidate = `${base}${sep}${i}`;
    if (!taken.includes(candidate)) return candidate;
  }
  return `${base}${sep}${Date.now()}`;
}

/**
 * Board positions are an i16 cell (contract: "Position is an integer cell", 710239f).
 *
 * The engine rounds and clamps on the way in and returns what it stored; this does the SAME
 * arithmetic before sending so the two can never disagree. A node that appears to save, is
 * rejected, and springs back on the next refetch is the worst shape a UI bug can have — the
 * operator sees success and gets none. Clamping means a far drag stops at the edge instead.
 */
export const POSITION_MIN = -32768;
export const POSITION_MAX = 32767;

export function clampCell(value: number): number {
  // NaN carries no direction, so it cannot be clamped toward anything — 0 is the only honest
  // answer. Infinity does carry one, and clamps to that bound like any other far drag.
  if (Number.isNaN(value)) return 0;
  return Math.min(POSITION_MAX, Math.max(POSITION_MIN, Math.round(value)));
}

export function clampPosition(position: { x: number; y: number }): { x: number; y: number } {
  return { x: clampCell(position.x), y: clampCell(position.y) };
}
