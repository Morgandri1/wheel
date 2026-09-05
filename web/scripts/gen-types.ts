/**
 * Generates TypeScript from the engine's exported JSON Schema.
 *
 *   pnpm gen:types           rewrite src/lib/schema/generated.ts
 *   pnpm gen:types --check   fail if it is out of date (for make check / CI)
 *
 * docs/schema is produced by `cargo run -p wheel-core --bin export-schema`, so this is how the
 * board's types follow wheel-core rather than drifting from it. wire-matrix.json is skipped: it
 * is a data document, not a schema, and is consumed directly by the conformance test.
 */
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { compile, type JSONSchema } from "json-schema-to-typescript";

const SCHEMA_DIR = resolve(process.cwd(), "../docs/schema");
const OUT = resolve(process.cwd(), "src/lib/schema/generated.ts");
const SKIP = new Set(["wire-matrix.json"]);

const BANNER = `/* eslint-disable */
/**
 * GENERATED — do not edit. Run \`pnpm gen:types\` after the engine re-exports docs/schema.
 * Source: wheel-core via docs/schema/*.json.
 */
`;

async function render(): Promise<string> {
  const files = readdirSync(SCHEMA_DIR)
    .filter((f) => f.endsWith(".json") && !SKIP.has(f))
    .sort();

  const blocks: string[] = [];
  for (const file of files) {
    const schema = JSON.parse(readFileSync(resolve(SCHEMA_DIR, file), "utf8")) as JSONSchema;
    blocks.push(
      await compile(schema, schema.title ?? file.replace(/\.json$/, ""), {
        bannerComment: "",
        additionalProperties: false,
        style: { singleQuote: false, semi: true, printWidth: 100 },
      }),
    );
  }

  return BANNER + dedupe(blocks, files) + "\n";
}

/**
 * Every schema file inlines the shared definitions it references, so NodeType, WireType and
 * friends come back once per file. Keep the first declaration of each name and drop identical
 * repeats; a repeat whose body differs is a genuine disagreement between two exports and is
 * loud rather than silently resolved.
 */
function dedupe(blocks: string[], files: string[]): string {
  const seen = new Map<string, { body: string; file: string }>();
  const kept: string[] = [];
  const conflicts: string[] = [];

  blocks.forEach((block, i) => {
    for (const chunk of block.split(/\n(?=^\/\*\*$)|\n(?=^export )/m)) {
      const text = chunk.trim();
      if (!text) continue;

      const name = /export\s+(?:type|interface|const)\s+(\w+)/.exec(text)?.[1];
      if (!name) {
        kept.push(text);
        continue;
      }

      const previous = seen.get(name);
      if (!previous) {
        seen.set(name, { body: normalise(text), file: files[i]! });
        kept.push(text);
        continue;
      }
      if (previous.body !== normalise(text)) {
        conflicts.push(`${name}: ${previous.file} and ${files[i]} export different shapes`);
      }
    }
  });

  if (conflicts.length) {
    console.error("docs/schema disagrees with itself:");
    for (const c of conflicts) console.error(`  ${c}`);
    process.exit(1);
  }

  return kept.join("\n\n").trimEnd();
}

/** Comments and whitespace are noise when asking whether two declarations are the same type. */
function normalise(text: string): string {
  return text
    .replace(/\/\*\*[\s\S]*?\*\//g, "")
    .replace(/\/\/.*$/gm, "")
    .replace(/\s+/g, " ")
    .trim();
}

async function main() {
  const next = await render();

  if (!process.argv.includes("--check")) {
    writeFileSync(OUT, next);
    console.log(`wrote ${OUT}`);
    return;
  }

  let current = "";
  try {
    current = readFileSync(OUT, "utf8");
  } catch {
    /* a missing file counts as out of date */
  }
  if (current !== next) {
    console.error("src/lib/schema/generated.ts is out of date with docs/schema. Run: pnpm gen:types");
    process.exit(1);
  }
  console.log("schema types are up to date");
}

void main();
