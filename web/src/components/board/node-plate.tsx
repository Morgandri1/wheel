"use client";

import { Handle, Position, type NodeProps } from "@xyflow/react";
import { memo, useEffect, useRef, useState } from "react";
import { AGENT_STATUS_META, NODE_META } from "@/lib/node-meta";
import { validateNodeName } from "@/lib/validate";
import { Glyph } from "@/components/ui";
import type { WheelNode } from "@/lib/schema";

export interface PlateData extends Record<string, unknown> {
  node: WheelNode;
  takenNames: string[];
  onRename: (id: string, name: string) => void;
  onOpenLog: (id: string) => void;
}

function NodePlateInner({ data, selected }: NodeProps) {
  const { node, takenNames, onRename, onOpenLog } = data as PlateData;
  const meta = NODE_META[node.type];
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(node.name);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => setDraft(node.name), [node.name]);
  useEffect(() => {
    if (editing) inputRef.current?.select();
  }, [editing]);

  const commit = () => {
    const err = validateNodeName(draft, takenNames.filter((n) => n !== node.name));
    if (err) {
      setError(err);
      return;
    }
    setError(null);
    setEditing(false);
    if (draft !== node.name) onRename(node.id, draft);
  };

  const status = node.type === "agent" ? (node.state?.status ?? "stopped") : null;
  const statusMeta = status ? AGENT_STATUS_META[status] : null;

  return (
    <div
      data-testid={`node-${node.name}`}
      data-node-type={node.type}
      className="plate w-[208px] select-none"
      style={{
        borderColor: selected ? "var(--rule-strong)" : undefined,
        outline: selected ? "1px solid var(--wire-read)" : undefined,
        outlineOffset: "1px",
      }}
      onDoubleClick={() => node.type === "agent" && onOpenLog(node.id)}
    >
      <Handle
        type="target"
        position={Position.Left}
        className="!h-2.5 !w-2.5 !rounded-none !border !border-rule !bg-[var(--panel-2)]"
      />

      <div className="flex items-center gap-2 border-b border-rule px-2.5 py-2">
        <span style={{ color: meta.tint }} className="shrink-0">
          <Glyph path={meta.glyph} />
        </span>
        {editing ? (
          <input
            ref={inputRef}
            data-testid={`node-name-input-${node.name}`}
            className="ident min-w-0 flex-1 border-b border-[var(--wire-read)] bg-transparent text-ink outline-none"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={commit}
            onKeyDown={(e) => {
              if (e.key === "Enter") commit();
              if (e.key === "Escape") {
                setDraft(node.name);
                setError(null);
                setEditing(false);
              }
            }}
          />
        ) : (
          <button
            className="ident min-w-0 flex-1 truncate text-left text-ink"
            data-testid={`node-name-${node.name}`}
            onDoubleClick={(e) => {
              e.stopPropagation();
              setEditing(true);
            }}
            title="Double-click to rename"
          >
            {node.name}
          </button>
        )}
        <span className="text-micro text-ink-faint">{meta.label}</span>
      </div>

      <div className="px-2.5 py-2">
        {node.type === "agent" ? (
          <div className="flex items-center justify-between gap-2">
            <span
              data-testid={`node-${node.name}-status`}
              data-status={status}
              className="inline-flex items-center gap-1.5 text-micro"
              style={{ color: statusMeta!.color }}
            >
              <span className="relative flex h-1.5 w-1.5">
                {statusMeta!.pulse ? (
                  <span
                    className="absolute inline-flex h-full w-full animate-ping rounded-full opacity-60"
                    style={{ background: statusMeta!.color }}
                  />
                ) : null}
                <span
                  className="relative inline-flex h-1.5 w-1.5 rounded-full"
                  style={{ background: statusMeta!.color }}
                />
              </span>
              {statusMeta!.label}
            </span>
            <span
              data-testid={`node-${node.name}-harness`}
              className="border border-rule px-1.5 py-px text-micro text-ink-dim"
            >
              {node.config.harness === "claude" ? "Claude" : "Codex"}
            </span>
          </div>
        ) : node.type === "ctx" ? (
          <p className="line-clamp-2 text-micro text-ink-dim">
            {node.config.markdown.trim().split("\n").filter(Boolean)[0]?.replace(/^#+\s*/, "") ||
              "Empty — nothing gets injected yet."}
          </p>
        ) : node.type === "endpoint" ? (
          <p className="ident truncate text-micro text-ink-dim">
            {node.config.method} {node.config.path}
          </p>
        ) : node.type === "table" ? (
          <p className="text-micro text-ink-dim">
            {node.config.columns.length} column{node.config.columns.length === 1 ? "" : "s"}
          </p>
        ) : node.type === "script" ? (
          <p className="text-micro text-ink-dim">{node.config.language}</p>
        ) : node.type === "vault" ? (
          <p className="text-micro text-ink-dim">
            {node.config.keys.length} key{node.config.keys.length === 1 ? "" : "s"}
          </p>
        ) : node.type === "mcp" ? (
          <p className="ident truncate text-micro text-ink-dim">
            {node.config.transport === "stdio" ? node.config.command || "no command" : node.config.url || "no url"}
          </p>
        ) : node.type === "tool" ? (
          <p className="text-micro text-ink-dim">
            {node.config.operations.filter((o) => o.enabled).length} of{" "}
            {node.config.operations.length} operation
            {node.config.operations.length === 1 ? "" : "s"} enabled
          </p>
        ) : (
          <p className="text-micro text-ink-dim">Blob store</p>
        )}
      </div>

      {error ? (
        <p className="border-t border-rule px-2.5 py-1 text-micro text-[var(--danger)]">{error}</p>
      ) : null}

      <Handle
        type="source"
        position={Position.Right}
        className="!h-2.5 !w-2.5 !rounded-none !border !border-rule !bg-[var(--panel-2)]"
      />
    </div>
  );
}

/** Memoised on the node identity so a state tick on one agent doesn't re-render 200 plates. */
export const NodePlate = memo(NodePlateInner, (a, b) => {
  const pa = (a.data as PlateData).node;
  const pb = (b.data as PlateData).node;
  return (
    a.selected === b.selected &&
    pa === pb &&
    (a.data as PlateData).takenNames === (b.data as PlateData).takenNames
  );
});
