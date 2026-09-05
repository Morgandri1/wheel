import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { NODE_TYPES, WIRE_TYPES, type NodeType, type WireType } from "@/lib/schema";
import { WIRE_MATRIX, isWireAllowed } from "@/lib/wire-matrix";

/**
 * The UI's copy of the matrix must equal the engine's, cell for cell.
 *
 * docs/schema/wire-matrix.json is generated from `wheel_core::wire_allowed`, so this test fails
 * the moment SDK changes a rule and we don't — which is the only way a client-side permission
 * table stays honest. Hand-transcribing the table into a test would only prove we can copy twice.
 */
/** vitest runs with cwd = web/, so the repo's schema export is one level up. */
const schemaPath = (name: string) => resolve(process.cwd(), "../docs/schema", name);

const exported = JSON.parse(
  readFileSync(schemaPath("wire-matrix.json"), "utf8"),
) as { allowed: { from: NodeType; to: NodeType; type: WireType }[] };

const key = (f: string, t: string, w: string) => `${f}>${t}:${w}`;
const allowed = new Set(exported.allowed.map((r) => key(r.from, r.to, r.type)));

describe("wire matrix conforms to the engine's export", () => {
  it("agrees on all 9 × 9 × 3 cells", () => {
    const disagreements: string[] = [];
    for (const from of NODE_TYPES) {
      for (const to of NODE_TYPES) {
        for (const type of WIRE_TYPES) {
          const engine = allowed.has(key(from, to, type));
          if (isWireAllowed(from, to, type) !== engine) disagreements.push(key(from, to, type));
        }
      }
    }
    expect(disagreements).toEqual([]);
  });

  it("covers the nine node types the engine knows about", () => {
    const engineTypes = JSON.parse(
      readFileSync(schemaPath("node-type.json"), "utf8"),
    ) as { enum: NodeType[] };
    expect([...NODE_TYPES].sort()).toEqual([...engineTypes.enum].sort());
  });

  it("has no rule the engine would reject", () => {
    for (const rule of WIRE_MATRIX) {
      expect(allowed.has(key(rule.from, rule.to, rule.type)), key(rule.from, rule.to, rule.type)).toBe(
        true,
      );
    }
    expect(WIRE_MATRIX).toHaveLength(exported.allowed.length);
  });

  it("gives every legal wire the language the popover and inspector need", () => {
    for (const rule of WIRE_MATRIX) {
      expect(rule.label.length, `${rule.from}→${rule.to}`).toBeGreaterThan(3);
      expect(rule.outgoing.length).toBeGreaterThan(8);
      expect(rule.incoming.length).toBeGreaterThan(8);
      expect(rule.commands.length).toBeGreaterThan(0);
    }
  });
});
