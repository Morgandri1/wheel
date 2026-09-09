// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import type { Position, WheelNode } from "@/lib/schema";

/**
 * A dragged node renders from a local override until the board agrees with the server.
 * The override is seeded from the engine's own reply, never from what we sent it: the engine
 * rounds and clamps on the way in, so the value it stored is the only value worth drawing.
 */
export type Overrides = Record<string, Position>;

function isCell(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

/** The position the engine says it stored, falling back to ours only if it told us nothing. */
export function storedPosition(saved: Partial<WheelNode> | null | undefined, sent: Position): Position {
  const p = saved?.position;
  return p && isCell(p.x) && isCell(p.y) ? { x: p.x, y: p.y } : sent;
}

export function dropOverride(overrides: Overrides, nodeId: string): Overrides {
  if (!(nodeId in overrides)) return overrides;
  const next = { ...overrides };
  delete next[nodeId];
  return next;
}

/** Drop every override the board has caught up with, keeping object identity when none has. */
export function settleOverrides(overrides: Overrides, nodes: WheelNode[]): Overrides {
  let next: Overrides | null = null;
  for (const n of nodes) {
    const held = overrides[n.id];
    if (!held) continue;
    if (held.x === n.position.x && held.y === n.position.y) {
      next ??= { ...overrides };
      delete next[n.id];
    }
  }
  return next ?? overrides;
}
