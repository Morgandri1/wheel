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
  /** Select the node so the inspector's Authenticate panel is in front of the person. */
  onAuthenticate: (id: string) => void;
}

function NodePlateInner({ data, selected }: NodeProps) {
  const { node, takenNames, onRename, onOpenLog, onAuthenticate } = data as PlateData;
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
  /**
   * §4: an agent's name is embedded in every peer's preamble and in its own session, so the
   * engine answers 409 agent_running rather than renaming underneath it. Say so here instead
   * of letting someone type a new name and have it bounce.
   */
  const renameLock =
    status === "running" || status === "starting"
      ? `Stop ${node.name} before renaming it — its name is baked into every peer's prompt.`
      : null;
  const statusMeta = status ? AGENT_STATUS_META[status] : null;

  return (
    <div
      data-testid={`node-${node.name}`}
      data-node-type={node.type}
      className="plate relative w-[190px] select-none"
      style={{ borderColor: selected ? "var(--accent)" : undefined }}
      onDoubleClick={() => node.type === "agent" && onOpenLog(node.id)}
    >
      {/* Selection is four accent corner marks rather than a glow: the system draws, it does
          not light, and the marks read at any zoom. */}
      {selected
        ? ([
            "-left-[5px] -top-[5px]",
            "-right-[5px] -top-[5px]",
            "-left-[5px] -bottom-[5px]",
            "-right-[5px] -bottom-[5px]",
          ].map((at) => (
            <span
              key={at}
              aria-hidden
              className={`absolute ${at} h-2 w-2 bg-[var(--accent)]`}
            />
          )))
        : null}

      <Handle
        type="target"
        position={Position.Left}
        className="!h-2.5 !w-2.5 !rounded-none !border !border-rule !bg-[var(--panel-2)]"
      />

      <div className="flex items-center gap-2 border-b border-rule px-2.5 py-1.5">
        <span style={{ color: meta.tint }} className="shrink-0">
          <Glyph path={meta.glyph} size={13} />
        </span>
        <span
          className="shrink-0 text-[10px] font-semibold uppercase tracking-[0.14em]"
          style={{ color: meta.tint }}
        >
          {meta.label}
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
            className="ident min-w-0 flex-1 truncate text-left text-[13px] font-semibold text-ink"
            data-testid={`node-name-${node.name}`}
            onDoubleClick={(e) => {
              e.stopPropagation();
              if (renameLock) {
                setError(renameLock);
                return;
              }
              setError(null);
              setEditing(true);
            }}
            title={renameLock ?? "Double-click to rename"}
            aria-disabled={renameLock ? true : undefined}
            data-rename-locked={renameLock ? "true" : undefined}
          >
            {node.name}
          </button>
        )}
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
            {status === "needs_auth" ? (
              /* The one status where the plate can offer the fix rather than only report the
                 problem: one click puts the key field in front of the person. */
              <button
                data-testid={`node-${node.name}-authenticate`}
                onClick={(e) => {
                  e.stopPropagation();
                  onAuthenticate(node.id);
                }}
                className="border px-1.5 py-px text-micro"
                style={{ borderColor: "var(--danger)", color: "var(--danger)" }}
              >
                Authenticate
              </button>
            ) : (
              <span
                data-testid={`node-${node.name}-harness`}
                className="border border-rule px-1.5 py-px text-micro text-ink-dim"
              >
                {node.config.harness === "claude" ? "Claude" : "Codex"}
              </span>
            )}
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
            {(node.config.operations ?? []).filter((o) => o.enabled).length} of{" "}
            {(node.config.operations ?? []).length} operation
            {(node.config.operations ?? []).length === 1 ? "" : "s"} enabled
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
/**
 * Everything a plate actually draws, as one comparable string.
 *
 * Object identity is useless here: every board refetch parses fresh JSON, so `prev === next` is
 * false for all 200 nodes on every tick and the memo does nothing. Comparing the rendered fields
 * means one agent going idle re-renders one plate instead of the whole board.
 */
export function plateSignature(node: WheelNode): string {
  const state = node.type === "agent" ? `${node.state?.status}|${node.state?.last_error ?? ""}` : "";
  return `${node.id}|${node.name}|${node.type}|${state}|${summary(node)}`;
}

/** The one line of config each plate shows — and therefore the only config it depends on. */
function summary(node: WheelNode): string {
  switch (node.type) {
    case "agent":
      return node.config.harness;
    case "ctx":
      return node.config.markdown;
    case "endpoint":
      return `${node.config.method} ${node.config.path}`;
    case "table":
      return String((node.config.columns ?? []).length);
    case "script":
      return node.config.language;
    case "vault":
      return String((node.config.keys ?? []).length);
    case "mcp":
      return node.config.transport === "stdio" ? node.config.command : node.config.url;
    case "tool": {
      const ops = node.config.operations ?? [];
      return `${ops.filter((o) => o.enabled !== false).length}/${ops.length}`;
    }
    default:
      return "";
  }
}

export const NodePlate = memo(NodePlateInner, (a, b) => {
  const da = a.data as PlateData;
  const db = b.data as PlateData;
  return (
    a.selected === b.selected &&
    plateSignature(da.node) === plateSignature(db.node) &&
    // Only matters while renaming, and only for the node being renamed.
    da.takenNames.length === db.takenNames.length
  );
});
