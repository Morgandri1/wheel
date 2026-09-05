"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { NODE_META, PALETTE_ORDER } from "@/lib/node-meta";
import { Glyph } from "@/components/ui";
import type { NodeType, WheelNode } from "@/lib/schema";

export interface Command {
  id: string;
  label: string;
  hint: string;
  run: () => void;
  glyph?: string;
  tint?: string;
}

/**
 * Cmd+K. Two things live here: placing a node, and jumping to one — the two actions that
 * otherwise need a mouse and a scan of the canvas. Everything is filtered by one query, because
 * asking someone to first choose a category and then a thing is a category they did not ask for.
 */
export function CommandPalette({
  open,
  onClose,
  nodes,
  onPlace,
  onSelect,
}: {
  open: boolean;
  onClose: () => void;
  nodes: WheelNode[];
  onPlace: (type: NodeType) => void;
  onSelect: (id: string) => void;
}) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const commands = useMemo<Command[]>(() => {
    const place = PALETTE_ORDER.map((type) => {
      const meta = NODE_META[type];
      return {
        id: `place:${type}`,
        label: `Place ${meta.label.toLowerCase()}`,
        hint: meta.blurb,
        glyph: meta.glyph,
        tint: meta.tint,
        run: () => onPlace(type),
      };
    });

    const go = nodes.map((n) => {
      const meta = NODE_META[n.type];
      return {
        id: `go:${n.id}`,
        label: n.name,
        hint: `Go to this ${meta.label.toLowerCase()}`,
        glyph: meta.glyph,
        tint: meta.tint,
        run: () => onSelect(n.id),
      };
    });

    return [...go, ...place];
  }, [nodes, onPlace, onSelect]);

  const matches = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return commands.slice(0, 12);
    return commands
      .filter((c) => c.label.toLowerCase().includes(q) || c.hint.toLowerCase().includes(q))
      .slice(0, 12);
  }, [commands, query]);

  useEffect(() => {
    if (open) {
      setQuery("");
      setActive(0);
      inputRef.current?.focus();
    }
  }, [open]);

  useEffect(() => setActive(0), [query]);

  if (!open) return null;

  const choose = (i: number) => {
    const command = matches[i];
    if (!command) return;
    onClose();
    command.run();
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-[rgba(0,0,0,0.45)] p-4 pt-[12vh]"
      onMouseDown={(e) => e.target === e.currentTarget && onClose()}
    >
      <div className="plate w-full max-w-lg overflow-hidden" data-testid="command-palette">
        <input
          ref={inputRef}
          data-testid="command-palette-input"
          aria-label="Search nodes and actions"
          className="w-full border-b border-rule bg-transparent px-3 py-2.5 text-meta text-ink placeholder:text-ink-faint focus:outline-none"
          placeholder="Go to a node, or place one…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setActive((i) => (i + 1) % Math.max(matches.length, 1));
            }
            if (e.key === "ArrowUp") {
              e.preventDefault();
              setActive((i) => (i - 1 + matches.length) % Math.max(matches.length, 1));
            }
            if (e.key === "Enter") {
              e.preventDefault();
              choose(active);
            }
            if (e.key === "Escape") onClose();
          }}
        />

        {matches.length ? (
          <ul className="max-h-[46vh] overflow-y-auto">
            {matches.map((c, i) => (
              <li key={c.id}>
                <button
                  data-testid={`command-${c.id}`}
                  onMouseEnter={() => setActive(i)}
                  onClick={() => choose(i)}
                  className={`flex w-full items-center gap-2.5 px-3 py-2 text-left ${
                    i === active ? "bg-[var(--panel-2)]" : ""
                  }`}
                >
                  {c.glyph ? (
                    <span style={{ color: c.tint }}>
                      <Glyph path={c.glyph} />
                    </span>
                  ) : null}
                  <span className="min-w-0 flex-1">
                    <span className="ident block truncate text-meta text-ink">{c.label}</span>
                    <span className="block truncate text-micro text-ink-faint">{c.hint}</span>
                  </span>
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <p className="px-3 py-3 text-micro text-ink-faint" data-testid="command-palette-empty">
            Nothing matches “{query}”.
          </p>
        )}
      </div>
    </div>
  );
}
