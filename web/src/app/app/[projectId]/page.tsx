"use client";

import Link from "next/link";
import { use, useCallback, useEffect, useMemo } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { engineApi, projects as projectApi } from "@/lib/api";
import { connectEvents } from "@/lib/events";
import { useBoardStore } from "@/store/board";
import { Canvas } from "@/components/board/canvas";
import { Inspector } from "@/components/inspector";
import { AgentDrawer } from "@/components/drawer/agent-drawer";
import { Header } from "@/components/header";
import { Button, Empty, Skeleton } from "@/components/ui";
import { toastError } from "@/components/ui/toast";
import type { ConnectionStatus } from "@/lib/events";

const CONNECTION_LABEL: Record<ConnectionStatus, { text: string; color: string }> = {
  connecting: { text: "Connecting", color: "var(--wire-write)" },
  open: { text: "Live", color: "var(--live)" },
  reconnecting: { text: "Reconnecting", color: "var(--wire-write)" },
  closed: { text: "Offline", color: "var(--danger)" },
};

export default function BoardPage({ params }: { params: Promise<{ projectId: string }> }) {
  const { projectId } = use(params);
  const qc = useQueryClient();
  const api = useMemo(() => engineApi(projectId), [projectId]);

  const selectedNodeId = useBoardStore((s) => s.selectedNodeId);
  const connection = useBoardStore((s) => s.connection);
  const setConnection = useBoardStore((s) => s.setConnection);
  const applyEvents = useBoardStore((s) => s.applyEvents);
  const reset = useBoardStore((s) => s.reset);

  useEffect(() => () => reset(), [projectId, reset]);

  const project = useQuery({
    queryKey: ["project", projectId],
    queryFn: () => projectApi.get(projectId),
    refetchInterval: (q) => (q.state.data?.status === "starting" ? 1200 : false),
  });

  const running = project.data?.status === "running";

  const board = useQuery({
    queryKey: ["board", projectId],
    queryFn: () => api.board(),
    enabled: running,
  });

  const refetchBoard = useCallback(() => {
    void qc.invalidateQueries({ queryKey: ["board", projectId] });
  }, [qc, projectId]);

  // One socket per open board. State ticks refresh the board; the rest lands in the store.
  useEffect(() => {
    if (!running) return;
    return connectEvents(projectId, {
      onStatus: setConnection,
      onBatch: (events) => {
        const { stateChanged, boardChanged } = applyEvents(events);
        if (stateChanged || boardChanged) refetchBoard();
      },
    });
  }, [projectId, running, applyEvents, refetchBoard, setConnection]);

  const nodes = board.data?.nodes ?? [];
  const selected = nodes.find((n) => n.id === selectedNodeId) ?? null;
  const conn = CONNECTION_LABEL[connection];

  return (
    <div className="flex h-screen flex-col overflow-hidden">
      <Header>
        <div className="flex items-center gap-3">
          <Link href="/app" className="text-micro text-ink-faint hover:text-ink" data-testid="link-projects">
            Projects
          </Link>
          <span className="text-ink-faint">/</span>
          <span className="text-meta font-medium" data-testid="board-project-name">
            {project.data?.name ?? "…"}
          </span>
          {running ? (
            <span
              data-testid="conn-indicator"
              data-state={connection}
              className="inline-flex items-center gap-1.5 text-micro"
              style={{ color: conn.color }}
            >
              <span className="h-1.5 w-1.5 rounded-full" style={{ background: conn.color }} />
              {conn.text}
            </span>
          ) : null}
        </div>
      </Header>

      {project.isPending ? (
        <div className="flex flex-1 gap-px p-px">
          <Skeleton className="w-[172px]" />
          <Skeleton className="flex-1" />
          <Skeleton className="w-[360px]" />
        </div>
      ) : project.error ? (
        <div className="p-8">
          <Empty title="Can't open this project" body={(project.error as Error).message} />
        </div>
      ) : !running ? (
        <div className="flex flex-1 items-center justify-center p-8">
          <Empty
            title={project.data?.status === "starting" ? "Container starting" : "This project is stopped"}
            body={
              project.data?.status === "starting"
                ? "The container is coming up. The board appears as soon as the engine answers."
                : "The board lives inside the project's container. Start it to place nodes, wire them together and run agents."
            }
            action={
              project.data?.status === "starting" ? null : (
                <Button
                  tone="primary"
                  data-testid="btn-start-project"
                  onClick={async () => {
                    try {
                      await projectApi.start(projectId);
                      void qc.invalidateQueries({ queryKey: ["project", projectId] });
                    } catch (e) {
                      toastError(e, "Couldn't start the container.");
                    }
                  }}
                >
                  Start the project
                </Button>
              )
            }
          />
        </div>
      ) : board.isPending ? (
        <div className="flex flex-1 gap-px p-px">
          <Skeleton className="w-[172px]" />
          <Skeleton className="flex-1" />
          <Skeleton className="w-[360px]" />
        </div>
      ) : board.error ? (
        <div className="p-8">
          <Empty
            title="The engine isn't answering"
            body={(board.error as Error).message}
            action={<Button onClick={refetchBoard}>Try again</Button>}
          />
        </div>
      ) : (
        <>
          <div className="flex min-h-0 flex-1">
            <Canvas nodes={nodes} api={api} onChanged={refetchBoard} />
            <Inspector
              node={selected}
              nodes={nodes}
              project={board.data.project}
              api={api}
              projectId={projectId}
              onChanged={refetchBoard}
            />
          </div>
          <AgentDrawer nodes={nodes} api={api} projectId={projectId} />
        </>
      )}
    </div>
  );
}
