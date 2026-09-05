"use client";

import { BaseEdge, EdgeLabelRenderer, getBezierPath, type EdgeProps } from "@xyflow/react";
import { memo } from "react";
import { WIRE_META } from "@/lib/node-meta";
import type { WireType } from "@/lib/schema";

export interface WireData extends Record<string, unknown> {
  wireType: WireType;
  injection: boolean;
  fromName: string;
  toName: string;
  onRemove: () => void;
}

/**
 * Wire styling carries the type twice — colour and stroke — so the board stays readable
 * without relying on colour alone.
 *   read       thin solid
 *   write      thick solid
 *   send       dashed
 *   injection  doubled line with ticks (ctx → agent: prepended, not delivered)
 */
function WireEdgeInner({ id, sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition, data, selected }: EdgeProps) {
  const { wireType, injection, fromName, toName, onRemove } = data as WireData;
  const meta = WIRE_META[wireType];
  const [path, labelX, labelY] = getBezierPath({
    sourceX,
    sourceY,
    sourcePosition,
    targetX,
    targetY,
    targetPosition,
    curvature: 0.32,
  });

  const width = wireType === "write" ? 2.6 : 1.5;

  return (
    <>
      {injection ? (
        <>
          <BaseEdge
            id={id}
            path={path}
            style={{ stroke: meta.color, strokeWidth: 4.5, opacity: 0.28 }}
          />
          <BaseEdge
            id={`${id}-core`}
            path={path}
            style={{
              stroke: meta.color,
              strokeWidth: 1.4,
              strokeDasharray: "1 5",
              strokeLinecap: "round",
            }}
          />
        </>
      ) : (
        <BaseEdge
          id={id}
          path={path}
          style={{
            stroke: meta.color,
            strokeWidth: selected ? width + 1 : width,
            strokeDasharray: meta.dash === "0" ? undefined : meta.dash,
          }}
        />
      )}

      <EdgeLabelRenderer>
        <div
          data-testid={`wire-${fromName}-${toName}-${wireType}`}
          className="pointer-events-auto absolute -translate-x-1/2 -translate-y-1/2"
          style={{ transform: `translate(-50%,-50%) translate(${labelX}px,${labelY}px)` }}
        >
          <button
            onClick={onRemove}
            title={`Remove ${wireType} wire ${fromName} → ${toName}`}
            className="border bg-[var(--panel-1)] px-1.5 py-px text-micro leading-4 text-ink-dim transition-colors hover:text-ink"
            style={{ borderColor: meta.color, color: meta.color }}
          >
            {injection ? "inject" : meta.label}
          </button>
        </div>
      </EdgeLabelRenderer>
    </>
  );
}

export const WireEdge = memo(WireEdgeInner);
