// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import type { AgentNode } from "@/lib/schema";

type AgentConfig = AgentNode["config"];
export type Budget = NonNullable<AgentConfig["budget"]>;
export type Workspace = NonNullable<AgentConfig["workspaces"]>[number];

export const IDLE_TIMEOUT_DEFAULT = 300;

/**
 * An empty field means "unset", never zero.
 *
 * The distinction is the whole point: `max_usd: 0` is a budget of nothing, which stops the agent
 * on its first turn, while an absent budget means no cap. A parser that turns "" into 0 converts
 * "I did not fill this in" into "spend nothing" — a silent stop with a config that looks deliberate.
 */
export function parseOptionalNumber(raw: string): number | null | undefined {
  const t = raw.trim();
  if (t === "") return undefined;
  const n = Number(t);
  return Number.isFinite(n) && n >= 0 ? n : null;
}

/**
 * Clearing a field must send an explicit `null`, never `undefined`.
 *
 * `JSON.stringify` DROPS undefined keys, so `{...config, budget: undefined}` goes out as a body
 * with no `budget` at all. Under replace semantics that clears it; under merge semantics an absent
 * key means "leave unchanged", so the cap silently survives a user clearing the box. `null` means
 * the same thing under both: unset it. The schema types both fields as `| null` for exactly this.
 */
export type ParsedBudget = { ok: true; budget: Budget | null } | { ok: false; message: string };

export function buildBudget(turns: string, usd: string): ParsedBudget {
  const t = parseOptionalNumber(turns);
  const u = parseOptionalNumber(usd);
  if (t === null || u === null) {
    return { ok: false, message: "Budget must be a number, or empty for no cap." };
  }
  if (t === undefined && u === undefined) return { ok: true, budget: null };
  const budget: Budget = {};
  if (t !== undefined) budget.max_turns = Math.floor(t);
  if (u !== undefined) budget.max_usd = u;
  return { ok: true, budget };
}

/** Same rule as the budget: empty clears with an explicit null, not a missing key. */
export function buildIdleTimeout(raw: string): { ok: true; secs: number | null } | { ok: false; message: string } {
  const n = parseOptionalNumber(raw);
  if (n === null) {
    return { ok: false, message: "Idle timeout must be a number of seconds, or empty for the default." };
  }
  return { ok: true, secs: n === undefined ? null : Math.floor(n) };
}

/**
 * A workspace path must be relative and free of `..`: the engine materialises it under the
 * project's own data dir, so an absolute path or a traversal is asking to write outside the
 * tenant. Rejected here as well as engine-side, because a control that offers something the
 * engine will refuse is a control that lies.
 */
export function validateWorkspacePath(path: string): string | null {
  const t = path.trim();
  if (t === "") return "A workspace needs a path.";
  if (t.startsWith("/")) return "Use a path relative to the project, not an absolute one.";
  if (t.split("/").includes("..")) return "A workspace path cannot contain `..`.";
  if (!/^[A-Za-z0-9._/-]+$/.test(t)) return "Use letters, digits, dot, dash, underscore and /.";
  return null;
}

/** Drops the git block entirely when no url is given, rather than storing an empty one. */
export function buildWorkspace(path: string, gitUrl: string, gitRef: string): Workspace {
  const ws: Workspace = { path: path.trim() };
  const url = gitUrl.trim();
  if (url) {
    ws.git = gitRef.trim() ? { url, ref: gitRef.trim() } : { url };
  }
  return ws;
}
