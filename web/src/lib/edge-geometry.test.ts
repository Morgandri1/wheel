import { describe, expect, it } from "vitest";
import { borderPoint, floatingEndpoints, sideToward, type Box } from "./edge-geometry";

const box = (x: number, y: number, width = 100, height = 40): Box => ({ x, y, width, height });

describe("sideToward — the wire leaves by the side that faces the other node", () => {
  const a = box(0, 0);

  it("faces right for a node to the right, left for one to the left", () => {
    // The bug this fixes: the left case used to still leave on the right and loop around.
    expect(sideToward(a, { x: 500, y: 20 })).toBe("right");
    expect(sideToward(a, { x: -500, y: 20 })).toBe("left");
  });

  it("faces up or down only when the other node is genuinely above or below", () => {
    expect(sideToward(a, { x: 50, y: -400 })).toBe("top");
    expect(sideToward(a, { x: 50, y: 400 })).toBe("bottom");
  });

  it("prefers left/right for a shallow diagonal, because the plate is wide", () => {
    // 300 across, 40 down on a 100x40 plate: visually a sideways wire, not a downward one.
    expect(sideToward(a, { x: 350, y: 60 })).toBe("right");
  });

  it("is symmetric: swapping the two nodes mirrors the sides", () => {
    const b = box(400, 0);
    const e = floatingEndpoints(a, b);
    expect(e.sourceSide).toBe("right");
    expect(e.targetSide).toBe("left");
    const back = floatingEndpoints(b, a);
    expect(back.sourceSide).toBe("left");
    expect(back.targetSide).toBe("right");
  });
});

describe("borderPoint", () => {
  const a = box(0, 0, 100, 40);

  it("lands ON the border, never inside or beyond", () => {
    for (const toward of [{ x: 900, y: 20 }, { x: -900, y: 20 }, { x: 50, y: 900 }, { x: 50, y: -900 }]) {
      const p = borderPoint(a, toward);
      const onEdge =
        Math.abs(p.x - a.x) < 1e-9 || Math.abs(p.x - (a.x + 100)) < 1e-9 ||
        Math.abs(p.y - a.y) < 1e-9 || Math.abs(p.y - (a.y + 40)) < 1e-9;
      expect(onEdge).toBe(true);
    }
  });

  it("exits the middle of the facing edge for a node directly alongside", () => {
    expect(borderPoint(a, { x: 500, y: 20 })).toEqual({ x: 100, y: 20 });
    expect(borderPoint(a, { x: -500, y: 20 })).toEqual({ x: 0, y: 20 });
  });

  it("returns the centre for two nodes stacked exactly, rather than dividing by zero", () => {
    expect(borderPoint(a, { x: 50, y: 20 })).toEqual({ x: 50, y: 20 });
  });
});
