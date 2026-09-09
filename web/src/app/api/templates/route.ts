// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * `GET /api/templates` — the gallery's list, read from `public/workflow_templates` at REQUEST time.
 *
 * `docs/proposals/wow-templates.md` §5: request-time over build-time static generation, so the
 * gallery reflects exactly what's deployed without a second thing to remember ("did I rebuild").
 * `crates/wheel-api/tests/template_gate.rs` is the build-time authority that every SHIPPED file is
 * legal — a file that fails to parse here should not exist on `main`, so this route degrades a bad
 * file to "not listed" (logged server-side) rather than 500ing the whole gallery over one entry.
 *
 * Summaries only, on purpose: the full board is not sent until a template is opened, at which point
 * the client fetches `/workflow_templates/<slug>.json` directly — it's a plain static asset under
 * `public/`, so no second route is needed to serve the thing this one already read once to summarise.
 */
import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { NextResponse } from "next/server";
import { parseTemplateFile } from "@/lib/templates";

export interface TemplateSummary {
  slug: string;
  title: string;
  description: string;
  nodeCount: number;
  wireCount: number;
  warnings: string[];
  requiresCapabilities: { http: boolean };
}

function templatesDir(): string {
  return path.join(process.cwd(), "public", "workflow_templates");
}

export async function GET() {
  const dir = templatesDir();
  let entries: string[];
  try {
    entries = await readdir(dir);
  } catch {
    // No directory at all reads as an empty gallery, not an error — nothing has been dropped in yet.
    return NextResponse.json({ templates: [] });
  }

  const templates: TemplateSummary[] = [];
  for (const entry of entries) {
    if (!entry.endsWith(".json")) continue;
    const slug = entry.slice(0, -".json".length);
    try {
      const raw = await readFile(path.join(dir, entry), "utf8");
      const parsed = parseTemplateFile(JSON.parse(raw));
      if (parsed.status !== "ok") {
        console.error(`template ${entry} failed validation: ${parsed.problems.join("; ")}`);
        continue;
      }
      templates.push({
        slug,
        title: parsed.template.title,
        description: parsed.template.description,
        nodeCount: parsed.template.board.nodes.length,
        wireCount: parsed.template.board.wires.length,
        warnings: parsed.warnings,
        requiresCapabilities: parsed.template.requiresCapabilities,
      });
    } catch (e) {
      console.error(`template ${entry} could not be read: ${(e as Error).message}`);
    }
  }

  // Stable order for a gallery a person scans repeatedly — alphabetical by slug, not directory
  // listing order (which the OS does not guarantee is stable across reads).
  templates.sort((a, b) => a.slug.localeCompare(b.slug));
  return NextResponse.json({ templates });
}
