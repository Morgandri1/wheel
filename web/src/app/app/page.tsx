"use client";

import Link from "next/link";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { projects as api } from "@/lib/api";
import type { Project, ProjectStatus } from "@/lib/schema";
import { Header } from "@/components/header";
import { Button, Dialog, Empty, Field, Input, Pill, Skeleton, Toggle } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";

const STATUS: Record<ProjectStatus, { label: string; color: string; pulse: boolean }> = {
  stopped: { label: "Stopped", color: "var(--ink-faint)", pulse: false },
  starting: { label: "Starting", color: "var(--wire-write)", pulse: true },
  running: { label: "Running", color: "var(--live)", pulse: true },
  error: { label: "Error", color: "var(--danger)", pulse: false },
};

export default function ProjectsPage() {
  const qc = useQueryClient();
  const { data, isPending, error } = useQuery({
    queryKey: ["projects"],
    queryFn: api.list,
    refetchInterval: (q) =>
      (q.state.data ?? []).some((p) => p.status === "starting") ? 1200 : 8000,
  });

  const [creating, setCreating] = useState(false);
  /**
   * Creating a project is the first call that has to reach the sandbox host, so it is the first
   * one that can hang when the host is unavailable. API confirmed the shape: provisioning retries
   * eat ~33s and the caller then gets an edge 502, not our error envelope. A button that says
   * "Creating…" for half a minute reads as a frozen app, so after ten seconds we say what is
   * actually happening. This is a message, not a timeout — the request is still in flight and may
   * still succeed.
   */
  const [slowCreate, setSlowCreate] = useState(false);
  const [name, setName] = useState("");
  const [pendingDelete, setPendingDelete] = useState<Project | null>(null);
  const [confirmName, setConfirmName] = useState("");

  const invalidate = () => qc.invalidateQueries({ queryKey: ["projects"] });

  const create = useMutation({
    mutationFn: (n: string) => api.create(n),
    onMutate: () => setSlowCreate(false),
    onSuccess: (p) => {
      setCreating(false);
      setName("");
      invalidate();
      toast(`Created ${p.name}.`);
    },
    onError: (e) => toastError(e, "Couldn't create that project."),
    onSettled: () => setSlowCreate(false),
  });

  useEffect(() => {
    if (!create.isPending) return;
    const t = setTimeout(() => setSlowCreate(true), 10_000);
    return () => clearTimeout(t);
  }, [create.isPending]);

  const remove = useMutation({
    mutationFn: (id: string) => api.remove(id),
    onSuccess: () => {
      setPendingDelete(null);
      setConfirmName("");
      invalidate();
      toast("Project deleted.");
    },
    onError: (e) => toastError(e, "Couldn't delete that project."),
  });

  const lifecycle = useMutation({
    mutationFn: ({ id, action }: { id: string; action: "start" | "stop" }) => api[action](id),
    onSuccess: invalidate,
    onError: (e) => toastError(e),
  });

  const setHttp = useMutation({
    mutationFn: ({ id, http }: { id: string; http: boolean }) =>
      api.patch(id, { capabilities: { http } }),
    onSuccess: invalidate,
    onError: (e) => toastError(e, "Couldn't change that capability."),
  });

  return (
    <div className="flex min-h-screen flex-col">
      <Header />
      <main className="mx-auto w-full max-w-5xl flex-1 px-6 py-10">
        <div className="mb-8 flex items-end justify-between gap-4">
          <div>
            <h1 className="display text-h2">Projects</h1>
            <p className="mt-1 max-w-[62ch] text-meta text-ink-dim">
              Each project is its own container: one board, one database, agents that keep running
              after you close the tab.
            </p>
          </div>
          <Button tone="primary" data-testid="btn-new-project" onClick={() => setCreating(true)}>
            New project
          </Button>
        </div>

        {isPending ? (
          <div className="flex flex-col gap-px border border-rule">
            {[0, 1, 2].map((i) => (
              <Skeleton key={i} className="h-[74px] w-full" />
            ))}
          </div>
        ) : error ? (
          <Empty
            title="Can't reach the API"
            body={(error as Error).message}
            action={
              <Button onClick={() => qc.invalidateQueries({ queryKey: ["projects"] })}>Try again</Button>
            }
          />
        ) : !data?.length ? (
          <Empty
            title="No projects yet"
            body="A project gives you a container with a board in it. Drop an agent on the board, wire a context node into it, and it starts with that context every time."
            action={
              <Button tone="primary" data-testid="btn-new-project-empty" onClick={() => setCreating(true)}>
                Create your first project
              </Button>
            }
          />
        ) : (
          <ul className="flex flex-col border border-rule" data-testid="project-list">
            {data.map((p) => (
              <li
                key={p.id}
                data-testid={`project-${p.name}`}
                className="group flex items-center gap-4 border-b border-rule bg-[var(--panel-1)] px-4 py-3 last:border-b-0"
              >
                <div className="min-w-0 flex-1">
                  <Link
                    href={`/app/${p.id}`}
                    className="text-lead font-semibold hover:underline underline-offset-4"
                    data-testid={`project-link-${p.name}`}
                  >
                    {p.name}
                  </Link>
                  <div className="mt-1 flex items-center gap-4">
                    <Pill
                      color={STATUS[p.status].color}
                      pulse={STATUS[p.status].pulse}
                      testId={`project-status-${p.name}`}
                    >
                      {STATUS[p.status].label}
                    </Pill>
                    <span className="text-micro text-ink-faint">
                      Created {new Date(p.created_at).toLocaleDateString(undefined, {
                        day: "numeric",
                        month: "short",
                        year: "numeric",
                      })}
                    </span>
                  </div>
                </div>

                <div className="w-[190px] shrink-0">
                  <Toggle
                    checked={p.capabilities.http}
                    onChange={(http) => setHttp.mutate({ id: p.id, http })}
                    label="Public HTTP"
                    hint={p.capabilities.http ? "Endpoints are reachable" : "Endpoints return 403"}
                    testId={`project-http-${p.name}`}
                  />
                </div>

                <div className="flex shrink-0 items-center gap-1.5">
                  {p.status === "running" || p.status === "starting" ? (
                    <Button
                      size="sm"
                      data-testid={`btn-stop-${p.name}`}
                      onClick={() => lifecycle.mutate({ id: p.id, action: "stop" })}
                    >
                      Stop
                    </Button>
                  ) : (
                    <Button
                      size="sm"
                      data-testid={`btn-start-${p.name}`}
                      onClick={() => lifecycle.mutate({ id: p.id, action: "start" })}
                    >
                      Start
                    </Button>
                  )}
                  <Button
                    size="sm"
                    tone="danger"
                    data-testid={`btn-delete-${p.name}`}
                    onClick={() => setPendingDelete(p)}
                  >
                    Delete
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </main>

      <Dialog open={creating} onClose={() => setCreating(false)} title="New project" testId="dialog-new-project">
        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (name.trim()) create.mutate(name.trim());
          }}
          className="flex flex-col gap-4"
        >
          <Field label="Name" hint="Shown on the board and in the container name.">
            <Input
              data-testid="input-project-name"
              value={name}
              autoFocus
              onChange={(e) => setName(e.target.value)}
              placeholder="field-notes"
            />
          </Field>
          {slowCreate ? (
            <p className="text-micro text-ink-faint" data-testid="create-taking-long">
              This is taking longer than usual. The project service may be starting up — the
              request is still going, and nothing has been created twice.
            </p>
          ) : null}
          <div className="flex justify-end gap-2">
            <Button type="button" tone="ghost" onClick={() => setCreating(false)}>
              Cancel
            </Button>
            <Button
              type="submit"
              tone="primary"
              data-testid="btn-create-project"
              disabled={!name.trim() || create.isPending}
            >
              {create.isPending ? "Creating…" : "Create project"}
            </Button>
          </div>
        </form>
      </Dialog>

      <Dialog
        open={Boolean(pendingDelete)}
        onClose={() => {
          setPendingDelete(null);
          setConfirmName("");
        }}
        title={`Delete ${pendingDelete?.name ?? ""}`}
        testId="dialog-delete-project"
      >
        <p className="mb-4 text-meta text-ink-dim">
          This stops the container and removes its volume — the board, the database, every chest
          blob and vault secret. Type <span className="ident text-ink">{pendingDelete?.name}</span>{" "}
          to confirm.
        </p>
        <Input
          data-testid="input-confirm-delete"
          value={confirmName}
          autoFocus
          onChange={(e) => setConfirmName(e.target.value)}
        />
        <div className="mt-4 flex justify-end gap-2">
          <Button
            tone="ghost"
            onClick={() => {
              setPendingDelete(null);
              setConfirmName("");
            }}
          >
            Cancel
          </Button>
          <Button
            tone="danger"
            data-testid="btn-confirm-delete"
            disabled={confirmName !== pendingDelete?.name || remove.isPending}
            onClick={() => pendingDelete && remove.mutate(pendingDelete.id)}
          >
            {remove.isPending ? "Deleting…" : "Delete project"}
          </Button>
        </div>
      </Dialog>
    </div>
  );
}
