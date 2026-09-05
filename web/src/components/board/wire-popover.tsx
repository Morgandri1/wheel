"use client";

import { useEffect, useRef } from "react";
import { allowedWireTypes, impliesRead, wireRule } from "@/lib/wire-matrix";
import { WIRE_META } from "@/lib/node-meta";
import type { PendingWire } from "@/store/board";
import type { WireType } from "@/lib/schema";

/**
 * Offers only the wire types §3 permits between these two node types. An illegal pair never
 * reaches this popover — the connection is refused at the drag, with the reason.
 */
export function WirePopover({
  pending,
  onPick,
  onCancel,
}: {
  pending: PendingWire;
  onPick: (type: WireType) => void;
  onCancel: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const options = allowedWireTypes(pending.fromType, pending.toType);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onCancel();
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onCancel();
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onDown);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("mousedown", onDown);
    };
  }, [onCancel]);

  return (
    <div
      ref={ref}
      data-testid="wire-popover"
      className="plate fixed z-50 w-[290px] p-1"
      style={{ left: Math.min(pending.at.x, window.innerWidth - 310), top: pending.at.y }}
    >
      <p className="px-2 py-1.5 text-micro text-ink-faint">
        What may <span className="ident text-ink-dim">{pending.fromType}</span> do to{" "}
        <span className="ident text-ink-dim">{pending.toType}</span>?
      </p>
      <ul className="flex flex-col">
        {options.map((t) => {
          const rule = wireRule(pending.fromType, pending.toType, t)!;
          const meta = WIRE_META[t];
          return (
            <li key={t}>
              <button
                data-testid={`wire-option-${t}`}
                onClick={() => onPick(t)}
                className="flex w-full flex-col items-start gap-0.5 px-2 py-1.5 text-left transition-colors hover:bg-[var(--panel-2)]"
              >
                <span className="flex items-center gap-2">
                  <svg width="20" height="8" aria-hidden>
                    <line
                      x1="1"
                      y1="4"
                      x2="19"
                      y2="4"
                      stroke={meta.color}
                      strokeWidth={t === "write" ? 2.6 : 1.5}
                      strokeDasharray={meta.dash === "0" ? undefined : meta.dash}
                      strokeLinecap="round"
                    />
                  </svg>
                  <span className="text-meta" style={{ color: meta.color }}>
                    {meta.label}
                  </span>
                  {t === "write" && impliesRead(pending.fromType, pending.toType) ? (
                    <span className="text-micro text-ink-faint">includes read</span>
                  ) : null}
                </span>
                <span className="text-micro text-ink-dim">{rule.outgoing}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
