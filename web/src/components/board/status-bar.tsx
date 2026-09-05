"use client";

import { useMemo } from "react";
import { WIRE_META } from "@/lib/node-meta";
import { WIRE_TYPES } from "@/lib/schema";
import type { WheelNode } from "@/lib/schema";

/**
 * What the board contains, and how to read it. The wire legend lives here rather than floating on
 * the canvas because it is reference, not furniture — you look at it once, and then you want the
 * space back for the board.
 */
export function StatusBar({ nodes }: { nodes: WheelNode[] }) {
  const counts = useMemo(() => {
    const agents = nodes.filter((n) => n.type === "agent");
    return {
      agents: agents.length,
      parked: agents.filter((n) => n.state?.status === "parked").length,
      running: agents.filter((n) => n.state?.status === "running").length,
      nodes: nodes.length,
      wires: nodes.reduce((total, n) => total + (n.wires ?? []).length, 0),
    };
  }, [nodes]);

  const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

  return (
    <footer
      data-testid="board-status-bar"
      className="flex h-8 shrink-0 items-center gap-5 border-t-2 border-rule bg-[var(--panel-1)] px-4 text-micro text-ink-dim"
    >
      <span data-testid="status-counts">
        {plural(counts.agents, "agent")}
        {counts.running ? ` · ${counts.running} running` : ""}
        {counts.parked ? ` · ${counts.parked} parked` : ""} · {plural(counts.nodes, "node")} ·{" "}
        {plural(counts.wires, "wire")}
      </span>

      <span className="flex-1" />

      {WIRE_TYPES.map((t) => (
        <span key={t} className="inline-flex items-center gap-1.5" data-testid={`legend-${t}`}>
          <svg width="18" height="4" aria-hidden>
            <line
              x1="0"
              y1="2"
              x2="18"
              y2="2"
              stroke={WIRE_META[t].color}
              strokeWidth="2"
              strokeDasharray={WIRE_META[t].dash === "0" ? undefined : WIRE_META[t].dash}
            />
          </svg>
          {WIRE_META[t].label}
        </span>
      ))}
    </footer>
  );
}
