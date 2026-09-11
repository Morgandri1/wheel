// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * The browser must never learn where the API is, or hold anything that authenticates to it.
 *
 * `import "server-only"` makes the BUILD fail when a client module imports a server module; that
 * is the gate. This is the tripwire beside it (§0b: a source-grep is a tripwire, not a gate): it
 * walks the module graph from every "use client" file the way the bundler does, and fails if
 * anything the browser loads names the API's address or its env vars, a server credential, the
 * session header or a ws-ticket — or imports a module marked server-only.
 */
const SRC = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const FORBIDDEN: [RegExp, string][] = [
  [/NEXT_PUBLIC_API_URL/, "the build-time API address"],
  [/\bWHEEL_API_URL\b/, "the server's API address"],
  [/\b(?:server)?[aA]piBaseUrl\b/, "an API base-URL helper"],
  [/\b(?:NEXT_PUBLIC|WHEEL)_DEV_TOKEN\b/, "a dev credential"],
  [/["'`]x-auth-token["'`]/i, "the session header, which only the server sets"],
  [/\bws-ticket\b/, "a WebSocket ticket, which the web no longer mints"],
];
const SERVER_ONLY = /^\s*import\s+["']server-only["'];?\s*$/m;

function sourceFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) return sourceFiles(full);
    return /\.tsx?$/.test(full) && !/\.test\.tsx?$/.test(full) && !full.endsWith(".d.ts") ? [full] : [];
  });
}

function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:"'`\\])\/\/.*$/gm, "$1");
}

function runtimeImports(source: string): string[] {
  const patterns = [
    /^\s*import\s+(?!type\b)(?:[^"'`;]*?\sfrom\s+)?["']([^"']+)["']/gm,
    /^\s*export\s+(?!type\b)[^"'`;]*?\sfrom\s+["']([^"']+)["']/gm,
    /\bimport\(\s*["']([^"']+)["']\s*\)/g,
  ];
  return patterns.flatMap((pattern) => [...source.matchAll(pattern)].map((m) => m[1]!));
}

function resolveImport(spec: string, from: string): string | null {
  const base = spec.startsWith("@/") ? join(SRC, spec.slice(2)) : spec.startsWith(".") ? resolve(dirname(from), spec) : null;
  if (!base) return null;
  const candidates = [base, `${base}.ts`, `${base}.tsx`, join(base, "index.ts"), join(base, "index.tsx")];
  return candidates.find((c) => existsSync(c) && statSync(c).isFile()) ?? null;
}

function clientGraph(): Map<string, string> {
  const graph = new Map<string, string>();
  const queue = sourceFiles(SRC).filter((f) => /^\s*["']use client["']/.test(readFileSync(f, "utf8")));
  while (queue.length) {
    const file = queue.pop()!;
    if (graph.has(file)) continue;
    const source = readFileSync(file, "utf8");
    graph.set(file, source);
    for (const spec of runtimeImports(stripComments(source))) {
      const target = resolveImport(spec, file);
      if (target && !graph.has(target)) queue.push(target);
    }
  }
  return graph;
}

const graph = clientGraph();
const rel = (file: string) => relative(SRC, file);

describe("what the browser loads", () => {
  it("is the graph the bundler builds — the instrument finds the modules it has to", () => {
    const names = [...graph.keys()].map(rel);
    for (const expected of [
      "lib/api.ts",
      "lib/events.ts",
      "lib/local-auth.ts",
      "lib/auth.ts",
      "lib/endpoint-probe.ts",
      "components/runtime-config.tsx",
    ]) {
      expect(names).toContain(expected);
    }
    expect(names.length).toBeGreaterThan(30);
  });

  it("names no API address, server credential, session header or ticket", () => {
    const offenders = [...graph].flatMap(([file, source]) =>
      FORBIDDEN.filter(([pattern]) => pattern.test(stripComments(source))).map(([, what]) => `${rel(file)}: ${what}`),
    );
    expect(offenders).toEqual([]);
  });

  it("imports nothing marked server-only", () => {
    expect([...graph].filter(([, source]) => SERVER_ONLY.test(source)).map(([file]) => rel(file))).toEqual([]);
  });
});

describe("the server side of the line", () => {
  it("marks every module that reads the API address or a server credential server-only", () => {
    const readers = sourceFiles(SRC).filter((f) =>
      /process\.env\.(?:WHEEL_API_URL|NEXT_PUBLIC_API_URL|WHEEL_DEV_TOKEN)/.test(readFileSync(f, "utf8")),
    );
    expect(readers.map(rel)).toContain("lib/runtime-config.ts");
    expect(readers.filter((f) => !SERVER_ONLY.test(readFileSync(f, "utf8"))).map(rel)).toEqual([]);
  });
});

describe("the tripwire itself", () => {
  it.each([
    "const u = process.env.NEXT_PUBLIC_API_URL;",
    "const w = process.env.WHEEL_API_URL;",
    "fetch(`${apiBaseUrl()}/v1`)",
    'headers["x-auth-token"] = t;',
    "const t = process.env.WHEEL_DEV_TOKEN;",
    'request("/v1/projects/p1/ws-ticket")',
  ])("catches %s in code", (line) => {
    expect(FORBIDDEN.some(([pattern]) => pattern.test(stripComments(line)))).toBe(true);
  });

  it("does not trip on prose in a comment, or on the // of a URL in a string", () => {
    expect(stripComments('// the x-auth-token header, NEXT_PUBLIC_API_URL\nconst a = "https://x";')).toBe(
      '\nconst a = "https://x";',
    );
    expect(stripComments("/* WHEEL_API_URL */ const b = 1;")).toBe(" const b = 1;");
  });

  it("follows static, bare, re-exported, multi-line and dynamic imports, and skips type-only ones", () => {
    const source = [
      'import a from "./a";',
      'import { b } from "@/b";',
      'import "./c";',
      'export { d } from "./d";',
      'const e = import("./e");',
      'import type { F } from "./f";',
      'export type { G } from "./g";',
      "import {\n  h,\n} from \"./h\";",
    ].join("\n");
    expect(runtimeImports(source)).toEqual(["./a", "@/b", "./c", "./h", "./d", "./e"]);
  });
});
