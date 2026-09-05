import { describe, expect, it } from "vitest";
import { NODE_TYPES, WIRE_TYPES, type WireType } from "@/lib/schema";
import {
  WIRE_MATRIX,
  allowedWireRules,
  hasIncomingWires,
  impliesRead,
  isInjection,
  allowedWireTypes,
  canConnect,
  connectableTargets,
  explainDenial,
  hasOutgoingWires,
  impliesRead,
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

describe("write implies read", () => {
  it("says so on the keyspaces where §3 makes write the wider grant", () => {
    for (const to of ["ctx", "table", "chest"] as const) {
      expect(impliesRead("agent", to), `agent→${to}`).toBe(true);
    }
  });

  it("says nothing of the sort where write is not on offer at all", () => {
    expect(impliesRead("agent", "vault")).toBe(false);
    expect(impliesRead("agent", "agent")).toBe(false);
    expect(impliesRead("agent", "mcp")).toBe(false);
    expect(impliesRead("table", "agent")).toBe(false);
  });

  it("is not claimed for endpoint → table, where write is the only thing granted", () => {
    expect(allowedWireTypes("endpoint", "table")).toEqual(["write"]);
    expect(impliesRead("endpoint", "table")).toBe(false);
  });
});

describe("the helpers the popover and inspector call", () => {
  it("hands the popover a rule per legal type, with its own language", () => {
    const rules = allowedWireRules("agent", "ctx");
    expect(rules.map((r) => r.type)).toEqual(["read", "write"]);
    expect(new Set(rules.map((r) => r.grants)).size).toBe(2);
    expect(allowedWireRules("agent", "endpoint")).toEqual([]);
  });

  it("says which types can be wired TO at all", () => {
    expect(hasIncomingWires("agent")).toBe(true);
    expect(hasIncomingWires("vault")).toBe(true);
    expect(hasIncomingWires("tool")).toBe(true);
    // Nothing may point at an endpoint: hits come in from outside, they are not routed to it.
    expect(hasIncomingWires("endpoint")).toBe(false);
  });

  it("lists what a source can reach, without duplicates", () => {
    const targets = connectableTargets("agent");
    expect(new Set(targets).size).toBe(targets.length);
    expect(targets).toContain("tool");
    expect(targets).not.toContain("endpoint");
    expect(connectableTargets("vault")).toEqual([]);
  });

  it("reports write-implies-read only where §3 says so", () => {
    expect(impliesRead("agent", "ctx")).toBe(true);
    expect(impliesRead("agent", "table")).toBe(true);
    expect(impliesRead("agent", "chest")).toBe(true);
    // A vault has no write wire at all, so there is nothing for read to be implied by.
    expect(impliesRead("agent", "vault")).toBe(false);
    expect(impliesRead("endpoint", "table")).toBe(false);
  });

  it("marks exactly one wire in the whole matrix as an injection", () => {
    const injections = WIRE_MATRIX.filter((r) => isInjection(r.from, r.to, r.type));
    expect(injections).toHaveLength(1);
    expect(injections[0]).toMatchObject({ from: "ctx", to: "agent", type: "send" });
  });
});
