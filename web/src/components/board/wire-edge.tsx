"use client";

import {
  BaseEdge,
  EdgeLabelRenderer,
  Position,
  getBezierPath,
  useInternalNode,
  type EdgeProps,
} from "@xyflow/react";
import { memo } from "react";
import { WIRE_META } from "@/lib/node-meta";
import type { WireType } from "@/lib/schema";
import { floatingEndpoints, type Side } from "@/lib/edge-geometry";

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
const SIDE_TO_POSITION: Record<Side, Position> = {
  top: Position.Top,
  right: Position.Right,
  bottom: Position.Bottom,
  left: Position.Left,
};

function WireEdgeInner({
  id,
  source,
  target,
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
  data,
  selected,
}: EdgeProps) {
  const { wireType, injection, fromName, toName, onRemove } = data as WireData;
  const meta = WIRE_META[wireType];

  // Anchor each end to the side facing the other node, rather than always right-to-left. A node
  // dragged to the left of its source used to get a wire that looped around both plates.
  const sourceNode = useInternalNode(source);
  const targetNode = useInternalNode(target);
  const floating =
    sourceNode?.measured?.width && targetNode?.measured?.width
      ? floatingEndpoints(
          {
            x: sourceNode.internals.positionAbsolute.x,
            y: sourceNode.internals.positionAbsolute.y,
            width: sourceNode.measured.width,
            height: sourceNode.measured.height ?? 0,
          },
          {
            x: targetNode.internals.positionAbsolute.x,
            y: targetNode.internals.positionAbsolute.y,
            width: targetNode.measured.width,
            height: targetNode.measured.height ?? 0,
          },
        )
      : null;

  // Falls back to xyflow's own handle coordinates while a node is still unmeasured, so a wire
  // never vanishes waiting for geometry.
  const [path, labelX, labelY] = getBezierPath({
    sourceX: floating?.source.x ?? sourceX,
    sourceY: floating?.source.y ?? sourceY,
    sourcePosition: floating ? SIDE_TO_POSITION[floating.sourceSide] : sourcePosition,
    targetX: floating?.target.x ?? targetX,
    targetY: floating?.target.y ?? targetY,
    targetPosition: floating ? SIDE_TO_POSITION[floating.targetSide] : targetPosition,
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
        {/* No text on the wire: the legend names the types once, and repeating it per edge turned
            a board into a wall of words. The stroke still carries the type twice (colour and
            dash), and this stays as the hover/selection target so a wire can still be removed. */}
        <div
          data-testid={`wire-${fromName}-${toName}-${wireType}`}
          data-wire-type={wireType}
          className="group pointer-events-auto absolute -translate-x-1/2 -translate-y-1/2 p-2"
          style={{ transform: `translate(-50%,-50%) translate(${labelX}px,${labelY}px)` }}
        >
          <button
            onClick={onRemove}
            title={`Remove ${wireType} wire ${fromName} → ${toName}`}
            aria-label={`Remove ${wireType} wire ${fromName} to ${toName}`}
            // Invisible AND inert until revealed. An opacity-0 button still takes clicks, which
            // would mean deleting a wire by clicking something you cannot see — so visibility and
            // clickability are turned on together, never separately.
            className={`flex h-4 w-4 items-center justify-center border bg-[var(--panel-1)] text-micro leading-none transition-opacity focus-visible:pointer-events-auto focus-visible:opacity-100 group-hover:pointer-events-auto group-hover:opacity-100 ${
              selected ? "opacity-100" : "pointer-events-none opacity-0"
            }`}
            style={{ borderColor: meta.color, color: meta.color }}
          >
            ×
          </button>
        </div>
      </EdgeLabelRenderer>
    </>
  );
}

export const WireEdge = memo(WireEdgeInner);
