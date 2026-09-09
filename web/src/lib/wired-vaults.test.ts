// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it } from "vitest";
import { wiredVaults } from "./wired-vaults";
import type { WheelNode } from "@/lib/schema";

const n = (id: string, type: string, wires: { to: string; type: string }[] = []) =>
  ({ id, name: id, type, position: { x: 0, y: 0 }, wires, config: {} }) as unknown as WheelNode;

describe("wiredVaults", () => {
  it("offers a vault the node can read", () => {
    const from = n("t1", "tool", [{ to: "v1", type: "read" }]);
    expect(wiredVaults(from, [from, n("v1", "vault")]).map((v) => v.id)).toEqual(["v1"]);
  });

  it("never offers a vault reachable only by a non-read wire", () => {
    // A picker that offered this would build a config the engine refuses — or worse, one the
    // operator believes is wired.
    const from = n("e1", "endpoint", [{ to: "v1", type: "send" }]);
    expect(wiredVaults(from, [from, n("v1", "vault")])).toEqual([]);
  });

  it("ignores read wires to nodes that are not vaults", () => {
    const from = n("a1", "agent", [{ to: "c1", type: "read" }]);
    expect(wiredVaults(from, [from, n("c1", "ctx")])).toEqual([]);
  });

  it("consults the node it is GIVEN, not any other node's wires", () => {
    // The constraint that keeps this correct when the fourth caller arrives: direction is per node
    // type, so whose wires to read is an input, never an inference.
    const tool = n("t1", "tool", []);
    const endpoint = n("e1", "endpoint", [{ to: "v1", type: "read" }]);
    const all = [tool, endpoint, n("v1", "vault")];
    expect(wiredVaults(tool, all)).toEqual([]);
    expect(wiredVaults(endpoint, all).map((v) => v.id)).toEqual(["v1"]);
  });

  it("survives a node with no wires field at all", () => {
    const from = { id: "x", name: "x", type: "tool", position: { x: 0, y: 0 }, config: {} } as unknown as WheelNode;
    expect(wiredVaults(from, [from])).toEqual([]);
  });

  it("skips a wire pointing at a node that is not on the board", () => {
    const from = n("t1", "tool", [{ to: "gone", type: "read" }]);
    expect(wiredVaults(from, [from])).toEqual([]);
  });
});
