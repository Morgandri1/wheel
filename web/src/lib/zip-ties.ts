// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import type { Point } from "@/lib/edge-geometry";

/**
 * Zip ties gather two or more wires into one movable stretch where they run side by side.
 *
 * Purely visual: a tie never changes wiring, permissions or scheduling, and the engine never sees
 * one. Ties are personal — kept in this browser, per project (see `zip-tie-storage.ts`) — so any
 * member can tidy their own view without touching the shared board.
 */

export type Orientation = "horizontal" | "vertical";

export interface ZipTie {
  id: string;
  /** Wire ids (`from:to:type`), in lane order. A wire is in at most one tie. */
  edges: string[];
  orientation: Orientation;
  /** Centre of the shared stretch, in flow coordinates. */
  at: Point;
}

/** Tie `edgeIds` together, taking them out of any other tie; a tie left with one wire is dropped. */
export function tieEdges(
  ties: ZipTie[],
  edgeIds: string[],
  at: Point,
  orientation: Orientation,
  id: string,
): ZipTie[] {
  const edges = [...new Set(edgeIds)];
  if (edges.length < 2) return ties;
  const taken = new Set(edges);
  const rest = ties
    .map((t) => ({ ...t, edges: t.edges.filter((e) => !taken.has(e)) }))
    .filter((t) => t.edges.length >= 2);
  return [...rest, { id, edges, orientation, at }];
}

export function untie(ties: ZipTie[], id: string): ZipTie[] {
  return ties.filter((t) => t.id !== id);
}

export function moveTie(ties: ZipTie[], id: string, at: Point): ZipTie[] {
  return ties.map((t) => (t.id === id ? { ...t, at } : t));
}

export function reorientTie(ties: ZipTie[], id: string): ZipTie[] {
  return ties.map((t) =>
    t.id === id ? { ...t, orientation: t.orientation === "horizontal" ? "vertical" : "horizontal" } : t,
  );
}

/** Drop wires that no longer exist and any tie left below two. Same array back when unchanged. */
export function pruneTies(ties: ZipTie[], live: ReadonlySet<string>): ZipTie[] {
  let changed = false;
  const out: ZipTie[] = [];
  for (const t of ties) {
    const edges = t.edges.filter((e) => live.has(e));
    if (edges.length !== t.edges.length) changed = true;
    if (edges.length >= 2) out.push(edges.length === t.edges.length ? t : { ...t, edges });
  }
  return changed ? out : ties;
}

/**
 * What to draw, and whether the cleanup is safe to save. Stale wires are always hidden, but an
 * empty wire set is never trusted as "every wire was deleted": a board that is still loading or
 * briefly empty must not erase a person's ties.
 */
export function settleTies(stored: ZipTie[], live: ReadonlySet<string>) {
  const shown = pruneTies(stored, live);
  return { shown, save: live.size > 0 && shown !== stored };
}

// ---------------------------------------------------------------- shaping a new tie

/** A wire as a new tie sees it: where it starts and ends. */
export interface WireSpan {
  id: string;
  from: Point;
  to: Point;
}

/** Run the stretch the way the wires mostly go already, so tying bends them least. */
export function suggestOrientation(spans: WireSpan[]): Orientation {
  let dx = 0;
  let dy = 0;
  for (const s of spans) {
    dx += Math.abs(s.to.x - s.from.x);
    dy += Math.abs(s.to.y - s.from.y);
  }
  return dy > dx ? "vertical" : "horizontal";
}

/** Higher (or further-left) wires take the higher (or left) lane, so they enter without crossing. */
export function laneOrder(spans: WireSpan[], orientation: Orientation): string[] {
  const across = (s: WireSpan) => (orientation === "horizontal" ? s.from.y + s.to.y : s.from.x + s.to.x);
  return [...spans].sort((a, b) => across(a) - across(b) || a.id.localeCompare(b.id)).map((s) => s.id);
}

export function centreOf(spans: WireSpan[]): Point {
  const n = Math.max(1, spans.length);
  let x = 0;
  let y = 0;
  for (const s of spans) {
    x += (s.from.x + s.to.x) / 2;
    y += (s.from.y + s.to.y) / 2;
  }
  return { x: x / n, y: y / n };
}
