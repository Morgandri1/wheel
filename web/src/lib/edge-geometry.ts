export type Box = { x: number; y: number; width: number; height: number };
export type Point = { x: number; y: number };
export type Side = "top" | "right" | "bottom" | "left";

const centerOf = (b: Box): Point => ({ x: b.x + b.width / 2, y: b.y + b.height / 2 });

/**
 * Where the line between two node centres crosses the first node's border.
 *
 * Wires used to leave every node on its right and arrive on every node's left, so a node placed to
 * the LEFT of its source got a wire that looped around both plates to come back in. Anchoring to
 * the side that actually faces the other node removes the loop without moving anything.
 */
export function borderPoint(box: Box, toward: Point): Point {
  const c = centerOf(box);
  const dx = toward.x - c.x;
  const dy = toward.y - c.y;
  if (dx === 0 && dy === 0) return c;

  const hw = box.width / 2;
  const hh = box.height / 2;
  // Scale the direction vector until it touches the nearer pair of edges: whichever axis needs the
  // smaller scale is the side the line actually exits through.
  const scaleX = dx === 0 ? Infinity : hw / Math.abs(dx);
  const scaleY = dy === 0 ? Infinity : hh / Math.abs(dy);
  const s = Math.min(scaleX, scaleY);
  return { x: c.x + dx * s, y: c.y + dy * s };
}

/** Which side of the box that crossing sits on, with ties broken toward the horizontal. */
export function sideToward(box: Box, toward: Point): Side {
  const c = centerOf(box);
  const dx = toward.x - c.x;
  const dy = toward.y - c.y;
  const hw = box.width / 2;
  const hh = box.height / 2;
  if (dx === 0 && dy === 0) return "right";
  // Compare against the box's own aspect: a wide plate should still anchor left/right for a target
  // that is only slightly above it.
  if (Math.abs(dy) * hw > Math.abs(dx) * hh) return dy > 0 ? "bottom" : "top";
  return dx > 0 ? "right" : "left";
}

/** Both endpoints at once, each facing the other node. */
export function floatingEndpoints(source: Box, target: Box) {
  const sc = centerOf(source);
  const tc = centerOf(target);
  return {
    source: borderPoint(source, tc),
    target: borderPoint(target, sc),
    sourceSide: sideToward(source, tc),
    targetSide: sideToward(target, sc),
  };
}
