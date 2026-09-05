"use client";

import {
  Background,
  BackgroundVariant,
  Controls,
  MiniMap,
  ReactFlow,
  ReactFlowProvider,
  applyNodeChanges,
  useReactFlow,
  type Connection,
  type Edge,
  type Node as RFNode,
  type NodeChange,
} from "@xyflow/react";
import "@xyflow/react/dist/base.css";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { NodePlate, type PlateData } from "@/components/board/node-plate";
import { WireEdge, type WireData } from "@/components/board/wire-edge";
import { Palette } from "@/components/board/palette";
import { WirePopover } from "@/components/board/wire-popover";
import { CommandPalette } from "@/components/board/command-palette";
import { NODE_META } from "@/lib/node-meta";
import { canConnect, explainDenial, isInjection } from "@/lib/wire-matrix";
import { suggestName } from "@/lib/validate";
import { newNodeInput } from "@/lib/node-defaults";
import { useBoardStore } from "@/store/board";
import { toast, toastError } from "@/components/ui/toast";
import type { EngineApi } from "@/lib/api";
import type { NodeType, Position, WheelNode, WireType } from "@/lib/schema";

const nodeTypes = { plate: NodePlate };
const edgeTypes = { wire: WireEdge };

interface CanvasProps {
  nodes: WheelNode[];
  api: EngineApi;
  onChanged: () => void;
}

function CanvasInner({ nodes, api, onChanged }: CanvasProps) {
  const { screenToFlowPosition, getViewport, setCenter } = useReactFlow();
  const wrapper = useRef<HTMLDivElement>(null);
  const select = useBoardStore((s) => s.select);
  const selectedNodeId = useBoardStore((s) => s.selectedNodeId);
  const openTab = useBoardStore((s) => s.openTab);
  const pendingWire = useBoardStore((s) => s.pendingWire);
  const setPendingWire = useBoardStore((s) => s.setPendingWire);
  /** An engine refusal for the wire being drawn, kept beside the popover rather than in a toast. */
  const [wireError, setWireError] = useState<{ code: string; message: string } | null>(null);

  const byId = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes]);
  const takenNames = useMemo(() => nodes.map((n) => n.name), [nodes]);

  /** Positions being dragged right now; they win over the server's copy until drag ends. */
  const [dragged, setDragged] = useState<Record<string, Position>>({});
  const [confirmDelete, setConfirmDelete] = useState<WheelNode | null>(null);
  const [pendingTool, setPendingTool] = useState<{ position: Position } | null>(null);
  const [toolUrl, setToolUrl] = useState("");
  const [paletteOpen, setPaletteOpen] = useState(false);

  const rename = useCallback(
    async (id: string, name: string) => {
      try {
        await api.patchNode(id, { name });
        onChanged();
      } catch (e) {
        toastError(e, "Couldn't rename that node.");
        onChanged();
      }
    },
    [api, onChanged],
  );

  const rfNodes: RFNode[] = useMemo(
    () =>
      nodes.map((n) => ({
        id: n.id,
        type: "plate",
        position: dragged[n.id] ?? n.position,
        selected: n.id === selectedNodeId,
        data: {
          node: n,
          takenNames,
          onRename: rename,
          onOpenLog: openTab,
          onAuthenticate: select,
        } satisfies PlateData,
      })),
    [nodes, dragged, selectedNodeId, takenNames, rename, openTab, select],
  );

  const removeWire = useCallback(
    async (from: string, to: string, type: WireType) => {
      try {
        await api.deleteWire(from, to, type);
        onChanged();
      } catch (e) {
        toastError(e, "Couldn't remove that wire.");
      }
    },
    [api, onChanged],
  );

  const rfEdges: Edge[] = useMemo(() => {
    const out: Edge[] = [];
    for (const n of nodes) {
      for (const w of n.wires ?? []) {
        const target = byId.get(w.to);
        if (!target) continue;
        out.push({
          id: `${n.id}:${w.to}:${w.type}`,
          source: n.id,
          target: w.to,
          type: "wire",
          data: {
            wireType: w.type,
            injection: isInjection(n.type, target.type, w.type),
            fromName: n.name,
            toName: target.name,
            onRemove: () => removeWire(n.id, w.to, w.type),
          } satisfies WireData,
        });
      }
    }
    return out;
  }, [nodes, byId, removeWire]);

  const onNodesChange = useCallback(
    (changes: NodeChange[]) => {
      const positions: Record<string, Position> = {};
      for (const c of changes) {
        if (c.type === "position" && c.position) positions[c.id] = c.position;
        if (c.type === "select" && c.selected) select(c.id);
      }
      if (Object.keys(positions).length) setDragged((prev) => ({ ...prev, ...positions }));
      // Keep xyflow's internal bookkeeping (dimensions, z-index) in step.
      void applyNodeChanges(changes, rfNodes);
    },
    [rfNodes, select],
  );

  const onNodeDragStop = useCallback(
    async (_: unknown, node: RFNode) => {
      const position = { x: Math.round(node.position.x), y: Math.round(node.position.y) };
      try {
        await api.patchNode(node.id, { position });
      } catch (e) {
        toastError(e, "Couldn't save that position.");
      } finally {
        setDragged((prev) => {
          const next = { ...prev };
          delete next[node.id];
          return next;
        });
        onChanged();
      }
    },
    [api, onChanged],
  );

  /** Refuse an illegal pair at the drag, so the popover only ever offers legal wires. */
  const isValidConnection = useCallback(
    (c: Connection | Edge) => {
      const from = byId.get(c.source as string);
      const to = byId.get(c.target as string);
      if (!from || !to || from.id === to.id) return false;
      return canConnect(from.type, to.type);
    },
    [byId],
  );

  const onConnect = useCallback(
    (c: Connection) => {
      const from = byId.get(c.source);
      const to = byId.get(c.target);
      if (!from || !to) return;
      if (from.id === to.id) {
        toast("A node can't wire to itself.", "error");
        return;
      }
      if (!canConnect(from.type, to.type)) {
        toast(explainDenial(from.name, from.type, to.name, to.type), "error");
        return;
      }
      const rect = wrapper.current?.getBoundingClientRect();
      setWireError(null);
      setPendingWire({
        from: from.id,
        to: to.id,
        fromType: from.type,
        toType: to.type,
        at: { x: (rect?.left ?? 0) + 260, y: (rect?.top ?? 0) + 120 },
      });
    },
    [byId, setPendingWire],
  );

  const commitWire = useCallback(
    async (type: WireType) => {
      if (!pendingWire) return;
      const { from, to } = pendingWire;
      setWireError(null);
      try {
        await api.createWire(from, to, type);
        setPendingWire(null);
        onChanged();
      } catch (e) {
        // The engine is the authority. A refusal stays IN the popover rather than in a toast:
        // this is an error the person has to act on — pick a different vault, rename a key — and
        // a toast disappears while they are still reading it.
        const err = e as { code?: string; message?: string };
        setWireError({ code: err.code ?? "", message: err.message || "The engine rejected that wire." });
        onChanged();
      }
    },
    [api, onChanged, pendingWire, setPendingWire],
  );

  const create = useCallback(
    async (type: NodeType, position: Position, config?: Record<string, unknown>) => {
      const input = newNodeInput(type, suggestName(type, takenNames), {
        x: Math.round(position.x),
        y: Math.round(position.y),
      });
      try {
        const created = await api.createNode(
          config ? { ...input, config: { ...input.config, ...config } } : input,
        );
        onChanged();
        select(created.id);
      } catch (e) {
        toastError(e, `Couldn't place that ${NODE_META[type].label.toLowerCase()}.`);
      }
    },
    [api, onChanged, select, takenNames],
  );

  const place = useCallback(
    (type: NodeType, position: Position) => {
      // A tool with no base URL is not a tool, and the engine refuses one — so ask for it at
      // placement rather than creating something invalid and reporting the engine's 400.
      if (type === "tool") {
        setPendingTool({ position });
        return;
      }
      void create(type, position);
    },
    [create],
  );

  const onDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      const type = e.dataTransfer.getData("application/wheel-node-type") as NodeType;
      if (!type) return;
      void place(type, screenToFlowPosition({ x: e.clientX - 104, y: e.clientY - 28 }));
    },
    [place, screenToFlowPosition],
  );

  const placeCentre = useCallback(
    (type: NodeType) => {
      const rect = wrapper.current?.getBoundingClientRect();
      const vp = getViewport();
      const x = ((rect?.width ?? 800) / 2 - vp.x) / vp.zoom - 104;
      const y = ((rect?.height ?? 600) / 2 - vp.y) / vp.zoom - 28;
      void place(type, { x, y });
    },
    [getViewport, place],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = document.activeElement;
      const typing = Boolean(el && /^(INPUT|TEXTAREA)$/.test(el.tagName));

      // Cmd+K works even while typing — it is how you get out of wherever you are.
      if ((e.key === "k" || e.key === "K") && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setPaletteOpen((open) => !open);
        return;
      }

      // Esc backs out of exactly one thing at a time, innermost first, so it is predictable.
      if (e.key === "Escape") {
        if (paletteOpen) return; // the palette closes itself
        if (pendingWire) {
          setPendingWire(null);
          return;
        }
        if (confirmDelete) {
          setConfirmDelete(null);
          return;
        }
        if (pendingTool) {
          setPendingTool(null);
          setToolUrl("");
          return;
        }
        if (!typing && selectedNodeId) select(null);
        return;
      }

      if (typing) return;
      if ((e.key !== "Delete" && e.key !== "Backspace") || !selectedNodeId) return;
      const node = byId.get(selectedNodeId);
      if (!node) return;
      e.preventDefault();
      if (node.type === "table" || node.type === "chest" || node.type === "vault") {
        setConfirmDelete(node);
      } else {
        void (async () => {
          try {
            await api.deleteNode(node.id);
            select(null);
            onChanged();
          } catch (err) {
            toastError(err, "Couldn't delete that node.");
          }
        })();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [api, byId, onChanged, select, selectedNodeId, paletteOpen, pendingWire, confirmDelete, pendingTool, setPendingWire]);

  return (
    <div className="flex min-h-0 flex-1">
      <Palette onPlace={placeCentre} />
      <div ref={wrapper} className="relative min-w-0 flex-1" data-testid="board-canvas">
        <ReactFlow
          nodes={rfNodes}
          edges={rfEdges}
          nodeTypes={nodeTypes}
          edgeTypes={edgeTypes}
          onNodesChange={onNodesChange}
          onNodeDragStop={onNodeDragStop}
          onConnect={onConnect}
          isValidConnection={isValidConnection}
          onPaneClick={() => select(null)}
          onDrop={onDrop}
          onDragOver={(e) => {
            e.preventDefault();
            e.dataTransfer.dropEffect = "copy";
          }}
          fitView
          fitViewOptions={{ padding: 0.24, maxZoom: 1 }}
          minZoom={0.2}
          maxZoom={1.8}
          proOptions={{ hideAttribution: true }}
          defaultEdgeOptions={{ type: "wire" }}
          connectionLineStyle={{ stroke: "var(--rule-strong)", strokeWidth: 1.5 }}
        >
          {/* A ruled grid, not dots — the system draws structure with lines. */}
          <Background variant={BackgroundVariant.Lines} gap={40} lineWidth={1} color="var(--grid-dot)" />
          <MiniMap
            pannable
            zoomable
            className="!border !border-rule !bg-[var(--panel-1)]"
            maskColor="color-mix(in srgb, var(--panel-0) 72%, transparent)"
            nodeColor={(n) => {
              const data = n.data as PlateData;
              return NODE_META[data.node.type].tint;
            }}
          />
          <Controls className="!border !border-rule !bg-[var(--panel-1)] [&_button]:!border-rule [&_button]:!bg-[var(--panel-1)] [&_button]:!fill-[var(--ink-dim)]" />
        </ReactFlow>

        {pendingWire ? (
          <WirePopover
            pending={pendingWire}
            error={wireError}
            onPick={commitWire}
            onCancel={() => {
              setPendingWire(null);
              setWireError(null);
            }}
          />
        ) : null}

        <CommandPalette
          open={paletteOpen}
          onClose={() => setPaletteOpen(false)}
          nodes={nodes}
          onPlace={placeCentre}
          onSelect={(id) => {
            select(id);
            const node = byId.get(id);
            if (node) setCenter(node.position.x + 104, node.position.y + 28, { zoom: 1, duration: 200 });
          }}
        />

        {pendingTool ? (
          <div className="plate absolute left-1/2 top-6 z-40 w-[380px] -translate-x-1/2 p-4" data-testid="tool-base-url-prompt">
            <p className="mb-2 text-meta">
              Where do this tool&apos;s requests go? Every operation is resolved against it.
            </p>
            <form
              onSubmit={(e) => {
                e.preventDefault();
                const url = toolUrl.trim();
                if (!/^https?:\/\/\S+$/.test(url)) return;
                const at = pendingTool.position;
                setPendingTool(null);
                setToolUrl("");
                void create("tool", at, { base_url: url });
              }}
            >
              <input
                autoFocus
                data-testid="input-tool-base-url"
                className="ident w-full rounded-control border border-rule bg-[var(--panel-0)] px-2.5 py-1.5 text-meta text-ink placeholder:text-ink-faint focus:border-[var(--wire-read)] focus:outline-none"
                placeholder="https://api.example.com"
                value={toolUrl}
                onChange={(e) => setToolUrl(e.target.value)}
              />
              <div className="mt-3 flex justify-end gap-2">
                <button
                  type="button"
                  className="h-7 rounded-control border border-transparent px-2.5 text-micro text-ink-dim hover:text-ink"
                  onClick={() => {
                    setPendingTool(null);
                    setToolUrl("");
                  }}
                >
                  Cancel
                </button>
                <button
                  type="submit"
                  data-testid="btn-place-tool"
                  disabled={!/^https?:\/\/\S+$/.test(toolUrl.trim())}
                  className="h-7 rounded-control border border-rule bg-[var(--panel-2)] px-2.5 text-micro disabled:opacity-40"
                >
                  Place tool
                </button>
              </div>
            </form>
          </div>
        ) : null}

        {confirmDelete ? (
          <div className="plate absolute left-1/2 top-6 z-40 w-[380px] -translate-x-1/2 p-4" data-testid="confirm-delete-node">
            <p className="text-meta">
              Deleting <span className="ident">{confirmDelete.name}</span> drops its data — the{" "}
              {confirmDelete.type === "table" ? "rows" : confirmDelete.type === "chest" ? "files" : "secrets"}{" "}
              go with it.
            </p>
            <div className="mt-3 flex justify-end gap-2">
              <button
                className="h-7 rounded-control border border-transparent px-2.5 text-micro text-ink-dim hover:text-ink"
                onClick={() => setConfirmDelete(null)}
              >
                Keep it
              </button>
              <button
                data-testid="btn-confirm-delete-node"
                className="h-7 rounded-control border px-2.5 text-micro"
                style={{ borderColor: "var(--danger)", color: "var(--danger)" }}
                onClick={async () => {
                  try {
                    await api.deleteNode(confirmDelete.id);
                    select(null);
                    onChanged();
                  } catch (e) {
                    toastError(e, "Couldn't delete that node.");
                  } finally {
                    setConfirmDelete(null);
                  }
                }}
              >
                Delete {confirmDelete.type}
              </button>
            </div>
          </div>
        ) : null}
      </div>
    </div>
  );
}

export function Canvas(props: CanvasProps) {
  return (
    <ReactFlowProvider>
      <CanvasInner {...props} />
    </ReactFlowProvider>
  );
}
