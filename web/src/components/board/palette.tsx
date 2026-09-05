"use client";

import { NODE_META, PALETTE_ORDER } from "@/lib/node-meta";
import { Glyph } from "@/components/ui";
import type { NodeType } from "@/lib/schema";

/**
 * Drag a type onto the canvas, or click it to drop one in the middle of the view.
 * Both paths end in the same place, so nobody has to discover drag-and-drop to get started.
 */
export function Palette({ onPlace }: { onPlace: (type: NodeType) => void }) {
  return (
    <aside
      data-testid="palette"
      className="flex w-[172px] shrink-0 flex-col border-r border-rule bg-[var(--panel-1)]"
    >
      <p className="border-b border-rule px-3 py-2 text-micro text-ink-faint">
        Drag onto the board, or click to place
      </p>
      <ul className="flex flex-col">
        {PALETTE_ORDER.map((type) => {
          const meta = NODE_META[type];
          return (
            <li key={type}>
              <button
                data-testid={`palette-${type}`}
                draggable
                onDragStart={(e) => {
                  e.dataTransfer.setData("application/wheel-node-type", type);
                  e.dataTransfer.effectAllowed = "copy";
                }}
                onClick={() => onPlace(type)}
                title={meta.blurb}
                className="group flex w-full items-center gap-2.5 border-b border-rule px-3 py-2 text-left transition-colors hover:bg-[var(--panel-2)]"
              >
                <span style={{ color: meta.tint }}>
                  <Glyph path={meta.glyph} />
                </span>
                <span className="text-meta text-ink">{meta.label}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </aside>
  );
}
