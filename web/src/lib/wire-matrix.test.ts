import { describe, expect, it } from "vitest";
import { NODE_TYPES, WIRE_TYPES, type WireType } from "@/lib/schema";
import {
  WIRE_MATRIX,
  allowedWireTypes,
  canConnect,
  connectableTargets,
  explainDenial,
  hasOutgoingWires,
  isWireAllowed,
  wireRule,
} from "@/lib/wire-matrix";

/**
 * The table itself is asserted against the engine's own export in
 * wire-matrix.conformance.test.ts. What is checked here is everything the UI layers on top:
 * ordering, the helpers the popover and inspector call, and the invariants that must hold
 * whatever SDK adds next.
 */
describe("wire matrix — shape of what the UI is offered", () => {
  it("offers types in read, write, send order wherever more than one is legal", () => {
    const order: WireType[] = ["read", "write", "send"];
    for (const from of NODE_TYPES) {
      for (const to of NODE_TYPES) {
        const got = allowedWireTypes(from, to);
        expect(got, `${from}→${to}`).toEqual([...got].sort((a, b) => order.indexOf(a) - order.indexOf(b)));
      }
    }
  });

  it("denies the overwhelming majority of the 9 × 9 × 3 grid", () => {
    const cells = NODE_TYPES.length * NODE_TYPES.length * WIRE_TYPES.length;
    const allowed = NODE_TYPES.flatMap((from) =>
      NODE_TYPES.flatMap((to) => allowedWireTypes(from, to)),
    );
    expect(allowed).toHaveLength(WIRE_MATRIX.length);
    expect(cells - allowed.length).toBe(cells - WIRE_MATRIX.length);
    // Default DENY is the whole point: a matrix that permitted even a fifth of the grid
    // would mean the UI had stopped being a meaningful guard.
    expect(allowed.length / cells).toBeLessThan(0.2);
  });

  it("never offers a node a wire to itself except between two agents", () => {
    for (const t of NODE_TYPES) {
      if (t === "agent") continue;
      expect(canConnect(t, t), t).toBe(false);
    }
  });
});

describe("wire matrix — structural invariants from §3", () => {
  it("gives ctx, table, vault, chest and mcp no outgoing wires except ctx → agent", () => {
    expect(hasOutgoingWires("table")).toBe(false);
    expect(hasOutgoingWires("vault")).toBe(false);
    expect(hasOutgoingWires("chest")).toBe(false);
    expect(hasOutgoingWires("mcp")).toBe(false);
    expect(connectableTargets("ctx")).toEqual(["agent"]);
    expect(allowedWireTypes("ctx", "agent")).toEqual(["send"]);
  });

  it("marks ctx → agent as the injection wire and nothing else", () => {
    const injections = WIRE_MATRIX.filter((r) => r.injection);
    expect(injections).toHaveLength(1);
    expect(injections[0]).toMatchObject({ from: "ctx", to: "agent", type: "send" });
  });

  it("has write imply read on ctx, table and chest", () => {
    for (const from of ["agent", "script"] as const) {
      for (const to of ["ctx", "table", "chest"] as const) {
        expect(wireRule(from, to, "write")?.implies).toEqual(["read"]);
      }
    }
  });

  it("never lets a node wire to a vault for writing — vault values are write-only via the API", () => {
    for (const from of NODE_TYPES) {
      expect(isWireAllowed(from, "vault", "write")).toBe(false);
      expect(isWireAllowed(from, "vault", "send")).toBe(false);
    }
  });

  it("never allows an endpoint to be a target", () => {
    for (const from of NODE_TYPES) {
      expect(canConnect(from, "endpoint")).toBe(false);
    }
  });

  it("gives a script the same data reach as an agent, minus script and mcp", () => {
    for (const to of ["ctx", "table", "chest", "vault"] as const) {
      expect(allowedWireTypes("script", to)).toEqual(allowedWireTypes("agent", to));
    }
    expect(canConnect("script", "script")).toBe(false);
    expect(canConnect("script", "mcp")).toBe(false);
  });

  it("gives every rule plain language in both directions and at least one command", () => {
    for (const rule of WIRE_MATRIX) {
      expect(rule.outgoing.length).toBeGreaterThan(0);
      expect(rule.incoming.length).toBeGreaterThan(0);
      expect(rule.commands.length).toBeGreaterThan(0);
      expect(rule.label.length).toBeGreaterThan(0);
    }
  });

  it("has no duplicate rules", () => {
    const keys = WIRE_MATRIX.map((r) => `${r.from}>${r.to}:${r.type}`);
    expect(new Set(keys).size).toBe(keys.length);
  });
});

describe("explainDenial", () => {
  it("says no wire is possible when the pair is entirely illegal", () => {
    expect(explainDenial("notes", "ctx", "vars", "vault", "read")).toBe(
      "No wire can go from ctx to vault. notes cannot reach vars.",
    );
  });

  it("names the legal alternatives when only the type is wrong", () => {
    expect(explainDenial("researcher", "agent", "notes", "ctx", "send")).toBe(
      "A agent cannot send a ctx. Legal here: read, write.",
    );
  });
});
