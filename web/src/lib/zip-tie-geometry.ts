// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { borderPoint, sideToward, type Box, type Point, type Side } from "@/lib/edge-geometry";
import type { Orientation, ZipTie } from "@/lib/zip-ties";

/** Distance between neighbouring lanes, and the length of the shared stretch. Flow pixels. */
export const LANE_GAP = 14;
export const STRETCH = 120;

/** One wire's place in a tie. */
export interface TieLane {
  orientation: Orientation;
  at: Point;
  /** Signed distance from the stretch's centre line. */
  offset: number;
}

export function laneOffset(index: number, count: number): number {
  return (index - (count - 1) / 2) * LANE_GAP;
}

/** How far a tie's band reaches across its lanes. */
export function bandLength(count: number): number {
  return Math.max(2, count) * LANE_GAP + 8;
}

export function lanesById(ties: ZipTie[]): Map<string, TieLane> {
  const out = new Map<string, TieLane>();
  for (const t of ties) {
    t.edges.forEach((e, i) =>
      out.set(e, { orientation: t.orientation, at: t.at, offset: laneOffset(i, t.edges.length) }),
    );
  }
  return out;
}

/** The lane's straight stretch, entered from the source's side so a wire never doubles back. */
export function stretchEnds(lane: TieLane, from: Point, to: Point): { entry: Point; exit: Point } {
  const half = STRETCH / 2;
  if (lane.orientation === "horizontal") {
    const y = lane.at.y + lane.offset;
    const dir = from.x <= to.x ? 1 : -1;
    return { entry: { x: lane.at.x - dir * half, y }, exit: { x: lane.at.x + dir * half, y } };
  }
  const x = lane.at.x + lane.offset;
  const dir = from.y <= to.y ? 1 : -1;
  return { entry: { x, y: lane.at.y - dir * half }, exit: { x, y: lane.at.y + dir * half } };
}

const SIDE_VECTOR: Record<Side, Point> = {
  top: { x: 0, y: -1 },
  right: { x: 1, y: 0 },
  bottom: { x: 0, y: 1 },
  left: { x: -1, y: 0 },
};

const centre = (b: Box): Point => ({ x: b.x + b.width / 2, y: b.y + b.height / 2 });
const reach = (a: Point, b: Point) => Math.min(160, Math.max(24, Math.hypot(b.x - a.x, b.y - a.y) / 2));
const round = (n: number) => Math.round(n * 10) / 10;
const pt = (p: Point) => `${round(p.x)},${round(p.y)}`;

/**
 * Source → curve into the lane → straight through the stretch → curve out → target. Each end
 * anchors on the side of its node facing the stretch, as untied wires do.
 */
export function tiedWirePath(source: Box, target: Box, lane: TieLane) {
  const { entry, exit } = stretchEnds(lane, centre(source), centre(target));
  const s = borderPoint(source, entry);
  const t = borderPoint(target, exit);
  const sv = SIDE_VECTOR[sideToward(source, entry)];
  const tv = SIDE_VECTOR[sideToward(target, exit)];
  const dir = { x: Math.sign(exit.x - entry.x), y: Math.sign(exit.y - entry.y) };
  const k1 = reach(s, entry);
  const k2 = reach(exit, t);
  const path =
    `M ${pt(s)} C ${pt({ x: s.x + sv.x * k1, y: s.y + sv.y * k1 })} ` +
    `${pt({ x: entry.x - dir.x * k1, y: entry.y - dir.y * k1 })} ${pt(entry)} ` +
    `L ${pt(exit)} C ${pt({ x: exit.x + dir.x * k2, y: exit.y + dir.y * k2 })} ` +
    `${pt({ x: t.x + tv.x * k2, y: t.y + tv.y * k2 })} ${pt(t)}`;
  return { path, entry, exit };
}
