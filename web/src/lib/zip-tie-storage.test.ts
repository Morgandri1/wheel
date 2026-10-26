// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, describe, expect, it, vi } from "vitest";
import { parseTies, readTies, serializeTies, writeTies } from "./zip-tie-storage";
import type { ZipTie } from "./zip-ties";

const tie = (id: string, edges: string[], over: Partial<ZipTie> = {}): ZipTie => ({
  id,
  edges,
  orientation: "horizontal",
  at: { x: 0, y: 0 },
  ...over,
});

afterEach(() => {
  vi.restoreAllMocks();
  window.localStorage.clear();
});

describe("zip tie storage", () => {
  it("round-trips per project", () => {
    const ties = [tie("t", ["a", "b"], { orientation: "vertical", at: { x: 1.5, y: -2 } })];
    expect(writeTies("p1", ties)).toBe(true);
    expect(readTies("p1")).toEqual(ties);
    expect(readTies("p2")).toEqual([]);
  });

  it("removes the key once the last tie goes", () => {
    writeTies("p1", [tie("t", ["a", "b"])]);
    writeTies("p1", []);
    expect(window.localStorage.length).toBe(0);
  });

  it("reports a refused write instead of pretending it saved", () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new DOMException("quota", "QuotaExceededError");
    });
    expect(writeTies("p1", [tie("t", ["a", "b"])])).toBe(false);
  });

  it("reads nothing from legacy, unversioned or unparseable values", () => {
    for (const raw of ["not json", "null", "[]", '{"ties":[]}', '{"v":2,"ties":[]}', '{"v":1,"ties":{}}']) {
      window.localStorage.setItem("wheel.zipTies.p", raw);
      expect(readTies("p")).toEqual([]);
    }
  });

  it("keeps the good ties and drops the malformed ones", () => {
    const good = tie("good", ["a", "b"]);
    const out = parseTies({
      v: 1,
      ties: [
        null,
        { id: "no-at", edges: ["x", "y"] },
        { id: "nan", edges: ["x", "y"], at: { x: NaN, y: 0 } },
        { id: "one", edges: ["x", 7, "x"], at: { x: 0, y: 0 } },
        good,
        { id: "good", edges: ["p", "q"], at: { x: 0, y: 0 } },
        { id: "steals", edges: ["a", "c", "d"], orientation: "diagonal", at: { x: 0, y: 0 } },
      ],
    });
    expect(out).toEqual([good, tie("steals", ["c", "d"])]);
  });

  it("serializes the versioned shape it reads", () => {
    const ties = [tie("t", ["a", "b"])];
    expect(parseTies(JSON.parse(serializeTies(ties)))).toEqual(ties);
  });
});
