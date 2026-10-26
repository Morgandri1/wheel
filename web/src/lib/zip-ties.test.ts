// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it } from "vitest";
import {
  centreOf,
  laneOrder,
  moveTie,
  pruneTies,
  reorientTie,
  settleTies,
  suggestOrientation,
  tieEdges,
  untie,
  type ZipTie,
} from "./zip-ties";

const tie = (id: string, edges: string[], over: Partial<ZipTie> = {}): ZipTie => ({
  id,
  edges,
  orientation: "horizontal",
  at: { x: 0, y: 0 },
  ...over,
});

describe("tieEdges", () => {
  it("needs at least two distinct wires", () => {
    const none: ZipTie[] = [];
    expect(tieEdges(none, ["a"], { x: 0, y: 0 }, "horizontal", "t")).toBe(none);
    expect(tieEdges(none, ["a", "a"], { x: 0, y: 0 }, "horizontal", "t")).toBe(none);
    expect(tieEdges(none, ["a", "b"], { x: 5, y: 6 }, "vertical", "t")).toEqual([
      tie("t", ["a", "b"], { orientation: "vertical", at: { x: 5, y: 6 } }),
    ]);
  });

  it("moves a wire out of its old tie, dropping a tie left with one wire", () => {
    const before = [tie("old", ["a", "b"]), tie("keep", ["c", "d", "e"])];
    const after = tieEdges(before, ["b", "e"], { x: 0, y: 0 }, "horizontal", "new");
    expect(after.map((t) => [t.id, t.edges])).toEqual([
      ["keep", ["c", "d"]],
      ["new", ["b", "e"]],
    ]);
  });
});

describe("editing one tie leaves the others alone", () => {
  const ties = [tie("a", ["1", "2"]), tie("b", ["3", "4"])];

  it("moves, reorients and removes only the named tie", () => {
    const moved = moveTie(ties, "a", { x: 9, y: 9 });
    expect(moved[0]!.at).toEqual({ x: 9, y: 9 });
    expect(moved[1]).toBe(ties[1]);
    expect(reorientTie(ties, "b")[1]!.orientation).toBe("vertical");
    expect(reorientTie(reorientTie(ties, "b"), "b")[1]!.orientation).toBe("horizontal");
    expect(untie(ties, "a")).toEqual([ties[1]]);
  });
});

describe("stale wires", () => {
  const ties = [tie("a", ["1", "2", "3"]), tie("b", ["4", "5"])];
  const all = new Set(["1", "2", "3", "4", "5"]);

  it("prune returns the same array when every wire still exists", () => {
    expect(pruneTies(ties, all)).toBe(ties);
  });

  it("prune drops deleted wires, and a tie that falls below two, without touching the rest", () => {
    expect(pruneTies(ties, new Set(["1", "3", "4"]))).toEqual([tie("a", ["1", "3"])]);
  });

  it("settle saves the cleanup once a wire is really gone", () => {
    const { shown, save } = settleTies(ties, new Set(["1", "2", "4", "5"]));
    expect(shown).toEqual([tie("a", ["1", "2"]), ties[1]]);
    expect(save).toBe(true);
  });

  it("settle never saves over ties while the board has no wires (loading or refetching)", () => {
    const { shown, save } = settleTies(ties, new Set());
    expect(shown).toEqual([]);
    expect(save).toBe(false);
  });

  it("settle has nothing to save when nothing is stale", () => {
    expect(settleTies(ties, all)).toEqual({ shown: ties, save: false });
  });
});

describe("shaping a new tie", () => {
  const spans = [
    { id: "low", from: { x: 0, y: 300 }, to: { x: 600, y: 320 } },
    { id: "high", from: { x: 0, y: 0 }, to: { x: 600, y: 40 } },
  ];

  it("runs along the wires' main direction", () => {
    expect(suggestOrientation(spans)).toBe("horizontal");
    expect(suggestOrientation([{ id: "x", from: { x: 0, y: 0 }, to: { x: 10, y: 500 } }])).toBe("vertical");
  });

  it("orders lanes by position, not by selection order", () => {
    expect(laneOrder(spans, "horizontal")).toEqual(["high", "low"]);
    expect(laneOrder([...spans].reverse(), "horizontal")).toEqual(["high", "low"]);
  });

  it("sits in the middle of the wires", () => {
    expect(centreOf(spans)).toEqual({ x: 300, y: 165 });
  });
});
