// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import type { ZipTie } from "@/lib/zip-ties";

/**
 * Ties are saved in this browser's localStorage, one key per project. They are personal: they do
 * not follow you to another browser and collaborators never see them, which is also why writing
 * them needs no project tier.
 */
const VERSION = 1;
const PREFIX = "wheel.zipTies.";

const finite = (n: unknown): n is number => typeof n === "number" && Number.isFinite(n);

/** Keep every well-formed tie and drop the rest: one bad entry must not cost the others. */
export function parseTies(raw: unknown): ZipTie[] {
  if (!raw || typeof raw !== "object" || (raw as { v?: unknown }).v !== VERSION) return [];
  const list = (raw as { ties?: unknown }).ties;
  if (!Array.isArray(list)) return [];
  const claimed = new Set<string>();
  const ids = new Set<string>();
  const out: ZipTie[] = [];
  for (const item of list) {
    if (!item || typeof item !== "object") continue;
    const t = item as Record<string, unknown>;
    const at = t.at as Record<string, unknown> | undefined;
    if (typeof t.id !== "string" || !t.id || ids.has(t.id) || !Array.isArray(t.edges)) continue;
    if (!at || !finite(at.x) || !finite(at.y)) continue;
    const edges = [...new Set(t.edges.filter((e): e is string => typeof e === "string" && !claimed.has(e)))];
    if (edges.length < 2) continue;
    edges.forEach((e) => claimed.add(e));
    ids.add(t.id);
    out.push({
      id: t.id,
      edges,
      orientation: t.orientation === "vertical" ? "vertical" : "horizontal",
      at: { x: at.x, y: at.y },
    });
  }
  return out;
}

export function serializeTies(ties: ZipTie[]): string {
  return JSON.stringify({ v: VERSION, ties });
}

/** Unreadable storage (private windows, blocked site data) reads as no ties. */
export function readTies(projectId: string): ZipTie[] {
  try {
    const raw = window.localStorage.getItem(PREFIX + projectId);
    return raw ? parseTies(JSON.parse(raw)) : [];
  } catch {
    return [];
  }
}

/** False when the browser refused the write, so the caller can say ties won't survive a reload. */
export function writeTies(projectId: string, ties: ZipTie[]): boolean {
  try {
    if (ties.length) window.localStorage.setItem(PREFIX + projectId, serializeTies(ties));
    else window.localStorage.removeItem(PREFIX + projectId);
    return true;
  } catch {
    return false;
  }
}
