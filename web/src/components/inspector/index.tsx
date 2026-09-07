"use client";

import { useEffect, useMemo, useState } from "react";
import { AGENT_STATUS_META, NODE_META } from "@/lib/node-meta";
import { Button, Field, Glyph, Input, Select, Textarea, Toggle } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";
import {
  IDLE_TIMEOUT_DEFAULT,
  buildBudget,
  buildWorkspace,
  buildIdleTimeout,
  validateWorkspacePath,
} from "@/lib/agent-config";
import { AuthFlow } from "@/components/inspector/auth-flow";
import { PanelBoundary } from "@/components/inspector/panel-boundary";
import { CtxPanel } from "@/components/inspector/ctx-panel";
import { EndpointPanel } from "@/components/inspector/endpoint-panel";
import { TablePanel } from "@/components/inspector/table-panel";
import { ToolPanel } from "@/components/inspector/tool-panel";
import { ScriptPanel } from "@/components/inspector/script-panel";
import { McpPanel } from "@/components/inspector/mcp-panel";
import { VaultPanel } from "@/components/inspector/vault-panel";
import { ChestPanel } from "@/components/inspector/chest-panel";
import { useBoardStore } from "@/store/board";
import { CollapseButton, SidebarRail } from "@/components/board/sidebar-rail";
import type { EngineApi } from "@/lib/api";
import type { AgentNode, Project, WheelNode } from "@/lib/schema";

export function Inspector({
  node,
  nodes,
  project,
  api,
  projectId,
  onChanged,
}: {
  node: WheelNode | null;
  nodes: WheelNode[];
  project: Project;
  api: EngineApi;
  projectId: string;
  onChanged: () => void;
}) {
  const collapsed = useBoardStore((s) => s.inspectorCollapsed);
  const toggle = useBoardStore((s) => s.toggleInspector);

  if (collapsed) {
    return (
      <SidebarRail side="right" label="Inspector" onExpand={toggle} testId="btn-inspector-expand" />
    );
  }

  if (!node) {
    return (
      <aside
        data-testid="inspector-empty"
        className="flex w-[360px] shrink-0 flex-col border-l border-rule bg-[var(--panel-1)] p-4"
      >
        <div className="mb-2 flex justify-end">
          <CollapseButton
            side="right"
            label="Inspector"
            onCollapse={toggle}
            testId="btn-inspector-collapse"
          />
        </div>
        <p className="text-meta text-ink-dim">
          Pick a node to see what it is and what it may touch. Drag from a node&apos;s edge to
          another node to wire them together.
        </p>
      </aside>
    );
  }

  const meta = NODE_META[node.type];
  return (
    <aside
      data-testid={`inspector-${node.type}`}
      className="flex w-[360px] shrink-0 flex-col overflow-y-auto border-l border-rule bg-[var(--panel-1)]"
    >
      <div className="flex items-center gap-2 border-b border-rule px-4 py-3">
        <span style={{ color: meta.tint }}>
          <Glyph path={meta.glyph} />
        </span>
        <span className="ident flex-1 truncate text-ink">{node.name}</span>
        <span className="text-micro text-ink-faint">{meta.label}</span>
        <CollapseButton
          side="right"
          label="Inspector"
          onCollapse={toggle}
          testId="btn-inspector-collapse"
        />
      </div>

      <div className="flex flex-col gap-5 p-4">
        {/* One panel's failure must not take the board with it — the node header above stays, so
            another node can be selected and the canvas keeps working. */}
        <PanelBoundary nodeName={node.name}>
        {node.type === "agent" ? (
          <AgentPanel node={node} nodes={nodes} api={api} onChanged={onChanged} />
        ) : node.type === "ctx" ? (
          <CtxPanel node={node} api={api} onChanged={onChanged} />
        ) : node.type === "endpoint" ? (
          <EndpointPanel
            node={node}
            nodes={nodes}
            project={project}
            api={api}
            onChanged={onChanged}
          />
        ) : node.type === "table" ? (
          <TablePanel node={node} api={api} projectId={projectId} onChanged={onChanged} />
        ) : node.type === "tool" ? (
          <ToolPanel
            node={node}
            nodes={nodes}
            api={api}
            projectId={projectId}
            onChanged={onChanged}
          />
        ) : node.type === "script" ? (
          <ScriptPanel node={node} api={api} onChanged={onChanged} />
        ) : node.type === "mcp" ? (
          <McpPanel node={node} api={api} onChanged={onChanged} />
        ) : node.type === "vault" ? (
          <VaultPanel node={node} api={api} onChanged={onChanged} />
        ) : (
          <ChestPanel node={node} api={api} projectId={projectId} />
        )}
        </PanelBoundary>
      </div>
    </aside>
  );
}

/**
 * The three agent fields that had no control at all: workspaces, budget and idle timeout.
 * Without them an agent can be edited in the UI but not CREATED there — a wheel-dev agent needs a
 * working directory and a spend cap, and both were hand-edit-only.
 *
 * Every save goes through `patchConfig`, which read-modify-writes the whole config. A partial
 * config write here would silently delete the fields it does not mention.
 */
function AgentRuntimeFields({
  node,
  patchConfig,
}: {
  node: AgentNode;
  patchConfig: (patch: Partial<AgentNode["config"]>) => Promise<void>;
}) {
  const [idle, setIdle] = useState(String(node.config.idle_timeout_secs ?? ""));
  const [turns, setTurns] = useState(String(node.config.budget?.max_turns ?? ""));
  const [usd, setUsd] = useState(String(node.config.budget?.max_usd ?? ""));
  const [wsPath, setWsPath] = useState("");
  const [wsUrl, setWsUrl] = useState("");
  const [wsRef, setWsRef] = useState("");
  const [wsError, setWsError] = useState<string | null>(null);

  useEffect(() => {
    setIdle(String(node.config.idle_timeout_secs ?? ""));
    setTurns(String(node.config.budget?.max_turns ?? ""));
    setUsd(String(node.config.budget?.max_usd ?? ""));
    setWsError(null);
  }, [node.id, node.config.idle_timeout_secs, node.config.budget]);

  const workspaces = node.config.workspaces ?? [];

  const saveBudget = async () => {
    const parsed = buildBudget(turns, usd);
    if (!parsed.ok) {
      toast(parsed.message, "error");
      return;
    }
    await patchConfig({ budget: parsed.budget });
  };

  const saveIdle = async () => {
    const parsed = buildIdleTimeout(idle);
    if (!parsed.ok) {
      toast(parsed.message, "error");
      return;
    }
    await patchConfig({ idle_timeout_secs: parsed.secs });
  };

  const addWorkspace = async () => {
    const problem = validateWorkspacePath(wsPath);
    setWsError(problem);
    if (problem) return;
    await patchConfig({ workspaces: [...workspaces, buildWorkspace(wsPath, wsUrl, wsRef)] });
    setWsPath("");
    setWsUrl("");
    setWsRef("");
  };

  const removeWorkspace = async (index: number) =>
    patchConfig({ workspaces: workspaces.filter((_, i) => i !== index) });

  return (
    <div className="flex flex-col gap-3 border-t border-rule pt-4">
      <Field
        label="Workspaces"
        hint="Directories the engine materialises under the project, cloned on first start. The first is the agent's working directory."
      >
        <div className="flex flex-col gap-1.5">
          {workspaces.map((ws, i) => (
            <div
              key={`${ws.path}-${i}`}
              className="flex items-center justify-between gap-2 border border-rule px-2 py-1.5 text-micro"
              data-testid="agent-workspace-row"
            >
              <span className="truncate font-mono">
                {ws.path}
                {ws.git ? <span className="text-ink-faint"> ← {ws.git.url}{ws.git.ref ? `#${ws.git.ref}` : ""}</span> : null}
              </span>
              <button
                type="button"
                className="shrink-0 text-ink-faint hover:text-[var(--danger)]"
                data-testid="btn-agent-workspace-remove"
                onClick={() => removeWorkspace(i)}
              >
                Remove
              </button>
            </div>
          ))}
          {workspaces.length === 0 ? (
            <p className="text-micro text-ink-faint">
              No workspace: the agent starts with no repository and no working directory of its own.
            </p>
          ) : null}
          <div className="flex flex-wrap gap-1.5">
            <Input
              value={wsPath}
              onChange={(e) => setWsPath(e.target.value)}
              placeholder="repos/wheel"
              data-testid="input-agent-workspace-path"
            />
            <Input
              value={wsUrl}
              onChange={(e) => setWsUrl(e.target.value)}
              placeholder="git url (optional)"
              data-testid="input-agent-workspace-url"
            />
            <Input
              value={wsRef}
              onChange={(e) => setWsRef(e.target.value)}
              placeholder="ref (optional)"
              data-testid="input-agent-workspace-ref"
            />
            <Button size="sm" data-testid="btn-agent-workspace-add" onClick={addWorkspace}>
              Add
            </Button>
          </div>
          {wsError ? (
            <p className="text-micro text-[var(--danger)]" data-testid="agent-workspace-error">
              {wsError}
            </p>
          ) : null}
        </div>
      </Field>

      <Field label="Budget" hint="Empty means no cap. The engine stops the agent at the limit with budget_exhausted.">
        <div className="flex gap-1.5">
          <Input
            value={turns}
            onChange={(e) => setTurns(e.target.value)}
            onBlur={saveBudget}
            placeholder="max turns"
            data-testid="input-agent-max-turns"
          />
          <Input
            value={usd}
            onChange={(e) => setUsd(e.target.value)}
            onBlur={saveBudget}
            placeholder="max USD"
            data-testid="input-agent-max-usd"
          />
        </div>
      </Field>

      <Field label="Idle timeout" hint={`Seconds before the process is parked and resumed on the next message. Empty uses the default of ${IDLE_TIMEOUT_DEFAULT}.`}>
        <Input
          value={idle}
          onChange={(e) => setIdle(e.target.value)}
          onBlur={saveIdle}
          placeholder={String(IDLE_TIMEOUT_DEFAULT)}
          data-testid="input-agent-idle-timeout"
        />
      </Field>
    </div>
  );
}

function AgentPanel({
  node,
  nodes,
  api,
  onChanged,
}: {
  node: AgentNode;
  nodes: WheelNode[];
  api: EngineApi;
  onChanged: () => void;
}) {
  const agent = useMemo(() => api.agent(node.id), [api, node.id]);
  // §3: a vault read wire is what puts a token in the agent's environment at spawn, so the wires
  // are where the answer to "which vault did this credential come from" actually lives.
  const wiredVaults = useMemo(
    () =>
      (node.wires ?? [])
        .filter((w) => w.type === "read")
        .map((w) => nodes.find((n) => n.id === w.to))
        .filter((n): n is WheelNode => n?.type === "vault")
        .map((n) => n.name),
    [node.wires, nodes],
  );
  const openTab = useBoardStore((s) => s.openTab);
  const select = useBoardStore((s) => s.select);
  /**
   * The next agent on this board still waiting for credentials. One login per agent is the
   * durable arrangement, so signing in a board means repeating this; handing over the next one
   * beats making someone find it on the canvas.
   */
  const nextNeedsAuth = useMemo(() => {
    const next = nodes.find(
      (n) => n.type === "agent" && n.id !== node.id && n.state?.status === "needs_auth",
    );
    return next ? { id: next.id, name: next.name } : null;
  }, [nodes, node.id]);

  const [prompt, setPrompt] = useState(node.config.system_prompt);
  const [model, setModel] = useState(node.config.model ?? "");
  const [saving, setSaving] = useState(false);

  useEffect(() => setPrompt(node.config.system_prompt), [node.id, node.config.system_prompt]);
  useEffect(() => setModel(node.config.model ?? ""), [node.id, node.config.model]);

  const status = node.state?.status ?? "stopped";
  const statusMeta = AGENT_STATUS_META[status];
  const dirty = prompt !== node.config.system_prompt || model !== (node.config.model ?? "");

  const patchConfig = async (patch: Partial<AgentNode["config"]>) => {
    try {
      await api.patchNode(node.id, { config: { ...node.config, ...patch } });
      onChanged();
    } catch (e) {
      toastError(e, "Couldn't save that.");
    }
  };

  const lifecycle = async (action: "start" | "stop" | "restart" | "clear") => {
    try {
      await agent[action]();
      onChanged();
      if (action === "start" || action === "restart") openTab(node.id);
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <>
      <div className="flex items-center justify-between">
        <span className="inline-flex items-center gap-1.5 text-meta" style={{ color: statusMeta.color }}>
          <span className="h-1.5 w-1.5 rounded-full" style={{ background: statusMeta.color }} />
          {statusMeta.label}
        </span>
        <div className="flex gap-1.5">
          {status === "stopped" || status === "error" || status === "needs_auth" ? (
            <Button size="sm" data-testid="btn-agent-start" onClick={() => lifecycle("start")}>
              Start
            </Button>
          ) : (
            <Button size="sm" data-testid="btn-agent-stop" onClick={() => lifecycle("stop")}>
              Stop
            </Button>
          )}
          <Button size="sm" data-testid="btn-agent-restart" onClick={() => lifecycle("restart")}>
            Restart
          </Button>
          <Button size="sm" data-testid="btn-agent-clear" onClick={() => lifecycle("clear")}>
            Clear
          </Button>
        </div>
      </div>

      {node.state?.last_error ? (
        <p className="border border-[color-mix(in_srgb,var(--danger)_45%,transparent)] px-2.5 py-2 text-micro text-[var(--danger)]">
          {node.state.last_error}
        </p>
      ) : null}

      <AuthFlow
        api={api}
        nodeId={node.id}
        needsAuth={status === "needs_auth"}
        vaults={wiredVaults}
        onAuthenticated={onChanged}
        nextNeedsAuth={nextNeedsAuth}
        onSelectAgent={select}
      />

      <Field label="Harness">
        <Select
          data-testid="inspector-agent-harness"
          value={node.config.harness}
          onChange={(e) => patchConfig({ harness: e.target.value as "claude" | "codex" })}
        >
          <option value="claude">Claude Code</option>
          <option value="codex">Codex</option>
        </Select>
      </Field>

      <Field label="Model" hint="Leave empty for the harness default.">
        <Input
          data-testid="inspector-agent-model"
          mono
          value={model}
          placeholder="claude-opus-5"
          onChange={(e) => setModel(e.target.value)}
        />
      </Field>

      <Field
        label="System prompt"
        hint="Applied on start and again after every context clear."
      >
        <Textarea
          data-testid="inspector-agent-system-prompt"
          rows={7}
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          placeholder="You gather sources and hand the writer a brief with links."
        />
      </Field>

      <div className="flex justify-end">
        <Button
          tone="primary"
          size="sm"
          data-testid="btn-agent-save"
          disabled={!dirty || saving}
          onClick={async () => {
            setSaving(true);
            await patchConfig({ system_prompt: prompt, model: model || undefined });
            setSaving(false);
            toast("Saved. It takes effect at the next start or clear.");
          }}
        >
          {saving ? "Saving…" : "Save"}
        </Button>
      </div>

      <div className="flex flex-col gap-3 border-t border-rule pt-4">
        <Toggle
          checked={node.config.run_on_startup ?? false}
          onChange={(v) => patchConfig({ run_on_startup: v })}
          label="Start with the project"
          hint="Comes up automatically whenever the container starts."
          testId="inspector-agent-run-on-startup"
        />
        <Toggle
          checked={node.config.ephemeral_context ?? false}
          onChange={(v) => patchConfig({ ephemeral_context: v })}
          label="Clear context after each turn"
          hint="Fresh session every message: system prompt and injected context are re-applied."
          testId="inspector-agent-ephemeral-context"
        />
      </div>

      <AgentRuntimeFields node={node} patchConfig={patchConfig} />

      <Button size="sm" data-testid="btn-open-log" onClick={() => openTab(node.id)}>
        Open log and chat
      </Button>
    </>
  );
}
