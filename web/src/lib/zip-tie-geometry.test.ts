// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it } from "vitest";
import type { Box } from "./edge-geometry";
import { LANE_GAP, STRETCH, lanesById, stretchEnds, tiedWirePath } from "./zip-tie-geometry";
import { moveTie, reorientTie, type ZipTie } from "./zip-ties";

const ties: ZipTie[] = [{ id: "t", edges: ["a", "b", "c"], orientation: "horizontal", at: { x: 400, y: 200 } }];
const lanes = lanesById(ties);
const ltr = { from: { x: 0, y: 0 }, to: { x: 900, y: 0 } };

describe("distinct parallel lanes", () => {
  it("gives every wire its own lane, LANE_GAP apart and centred on the tie", () => {
    expect(["a", "b", "c"].map((id) => lanes.get(id)!.offset)).toEqual([-LANE_GAP, 0, LANE_GAP]);
  });

  it("makes every stretch straight, parallel and the same length", () => {
    const ends = ["a", "b", "c"].map((id) => stretchEnds(lanes.get(id)!, ltr.from, ltr.to));
    for (const { entry, exit } of ends) {
      expect(entry.y).toBe(exit.y);
      expect(exit.x - entry.x).toBe(STRETCH);
    }
    expect(ends.map((e) => e.entry.y)).toEqual([186, 200, 214]);
  });

  it("follows the tie when it moves", () => {
    const moved = lanesById(moveTie(ties, "t", { x: 100, y: 500 })).get("b")!;
    expect(stretchEnds(moved, ltr.from, ltr.to)).toEqual({
      entry: { x: 100 - STRETCH / 2, y: 500 },
      exit: { x: 100 + STRETCH / 2, y: 500 },
    });
  });

  it("keeps direction: a right-to-left wire enters on the right", () => {
    const { entry, exit } = stretchEnds(lanes.get("a")!, ltr.to, ltr.from);
    expect(entry.x).toBeGreaterThan(exit.x);
  });

  it("stands the stretch upright when the tie is vertical", () => {
    const v = lanesById(reorientTie(ties, "t")).get("a")!;
    const { entry, exit } = stretchEnds(v, { x: 0, y: 0 }, { x: 0, y: 900 });
    expect(entry.x).toBe(400 - LANE_GAP);
    expect(exit.x).toBe(entry.x);
    expect(exit.y - entry.y).toBe(STRETCH);
  });
});

describe("tiedWirePath", () => {
  it("runs source border → stretch → target border", () => {
    const flat: ZipTie[] = [{ ...ties[0]!, at: { x: 450, y: 20 } }];
    const source: Box = { x: 0, y: 0, width: 100, height: 40 };
    const target: Box = { x: 800, y: 0, width: 100, height: 40 };
    const { path, entry, exit } = tiedWirePath(source, target, lanesById(flat).get("b")!);
    expect(path.startsWith("M 100,20 ")).toBe(true);
    expect(path).toContain(` ${entry.x},${entry.y} L ${exit.x},${exit.y} `);
    expect(path.endsWith(" 800,20")).toBe(true);
  });
});
