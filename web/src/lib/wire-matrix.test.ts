import { describe, expect, it } from "vitest";
import { NODE_TYPES, WIRE_TYPES, type NodeType, type WireType } from "@/lib/schema";
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
 * Transcribed by hand, a second time, straight from docs/ARCHITECTURE.md §3 —
 * deliberately NOT derived from WIRE_MATRIX, so a typo in the module fails here
 * instead of agreeing with itself.
 */
const EXPECTED: Record<string, WireType[]> = {
  "agent>agent": ["send"],
  "agent>ctx": ["read", "write"],
  "agent>table": ["read", "write"],
  "agent>vault": ["read"],
  "agent>chest": ["read", "write"],
  "agent>script": ["read"],
  "agent>mcp": ["read"],
  "ctx>agent": ["send"],
  "endpoint>agent": ["send"],
  "endpoint>table": ["write"],
  "endpoint>script": ["send"],
  "script>agent": ["send"],
  "script>ctx": ["read", "write"],
  "script>table": ["read", "write"],
  "script>chest": ["read", "write"],
  "script>vault": ["read"],
};

const expectedFor = (from: NodeType, to: NodeType): WireType[] => EXPECTED[`${from}>${to}`] ?? [];

describe("wire matrix — every cell of §3", () => {
  it("covers all 192 (from, to, type) combinations with no surprises", () => {
    const wrong: string[] = [];
    for (const from of NODE_TYPES) {
      for (const to of NODE_TYPES) {
        for (const type of WIRE_TYPES) {
          const want = expectedFor(from, to).includes(type);
          const got = isWireAllowed(from, to, type);
          if (want !== got) wrong.push(`${from} -${type}-> ${to}: expected ${want}, got ${got}`);
        }
      }
    }
    expect(wrong).toEqual([]);
  });

  it("orders allowed types read, write, send", () => {
    for (const from of NODE_TYPES) {
      for (const to of NODE_TYPES) {
        expect(allowedWireTypes(from, to)).toEqual(expectedFor(from, to));
      }
    }
  });

  it("denies by default — 170 of the 192 combinations are refused", () => {
    const allowed = NODE_TYPES.flatMap((from) =>
      NODE_TYPES.flatMap((to) => allowedWireTypes(from, to)),
    );
    // 8 types x 8 types x 3 wire types = 192 possibilities; §3 permits 22.
    expect(allowed).toHaveLength(22);
    expect(WIRE_MATRIX).toHaveLength(22);
    expect(NODE_TYPES.length * NODE_TYPES.length * WIRE_TYPES.length - allowed.length).toBe(170);
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
