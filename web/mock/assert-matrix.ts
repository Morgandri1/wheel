/**
 * The mock refuses to start if its wire matrix has drifted from the engine's export.
 *
 * QA's point: a mock that enforces a *copy* of the rules can quietly disagree with the engine
 * the moment SDK regenerates, and then everything developed against it is developed against a
 * fiction. The UI's matrix is already pinned to docs/schema/wire-matrix.json by
 * src/lib/wire-matrix.conformance.test.ts; this makes the same guarantee hold at runtime, so
 * you cannot boot a mock that would allow a wire the engine denies, or deny one it allows.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { isWireAllowed } from "@/lib/wire-matrix";
import { NODE_TYPES, WIRE_TYPES, type NodeType, type WireType } from "@/lib/schema";

interface ExportedMatrix {
  allowed: { from: NodeType; to: NodeType; type: WireType }[];
}

export function assertMatrixMatchesEngine(): number {
  const path = resolve(process.cwd(), "../docs/schema/wire-matrix.json");
  const exported = JSON.parse(readFileSync(path, "utf8")) as ExportedMatrix;
  const allowed = new Set(exported.allowed.map((r) => `${r.from}>${r.to}:${r.type}`));

  const disagreements: string[] = [];
  for (const from of NODE_TYPES) {
    for (const to of NODE_TYPES) {
      for (const type of WIRE_TYPES) {
        const key = `${from}>${to}:${type}`;
        if (isWireAllowed(from, to, type) !== allowed.has(key)) disagreements.push(key);
      }
    }
  }

  if (disagreements.length) {
    console.error(
      `mock refuses to start: its wire matrix disagrees with ${path} on ${disagreements.length} cell(s):`,
    );
    for (const d of disagreements) console.error(`  ${d}`);
    console.error("run: pnpm gen:types, then reconcile src/lib/wire-matrix.ts");
    process.exit(1);
  }

  return exported.allowed.length;
}
