/**
 * The mock plays the engine, so it enforces wires from the engine's OWN data:
 * docs/schema/wire-matrix.json, generated from wheel_core::wire_allowed.
 *
 * It deliberately does NOT go through src/lib/wire-matrix.ts. That module is the UI's copy —
 * pinned to this same file by a conformance test, but still a copy, and a copy is exactly what
 * produced BUG-004 one layer down. If the mock read the copy, then the moment SDK regenerated,
 * QA's E2E suite would be asserting against the UI's opinion of what is legal while the real
 * engine did something else. Reading the export removes the class of bug rather than detecting it.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { NodeType, WireType } from "@/lib/schema";

interface ExportedMatrix {
  allowed: { from: NodeType; to: NodeType; type: WireType }[];
}

const PATH = resolve(process.cwd(), "../docs/schema/wire-matrix.json");

const exported = JSON.parse(readFileSync(PATH, "utf8")) as ExportedMatrix;

const allowed = new Set(exported.allowed.map((r) => `${r.from}>${r.to}:${r.type}`));

export const allowedWireCount = exported.allowed.length;
export const matrixPath = PATH;

/** Default DENY, straight from the engine's export. */
export function engineAllowsWire(from: NodeType, to: NodeType, type: WireType): boolean {
  return allowed.has(`${from}>${to}:${type}`);
}
