"use client";

import { useEffect, useMemo, useState } from "react";
import { AGENT_STATUS_META, NODE_META } from "@/lib/node-meta";
import { Button, Field, Glyph, Input, Select, Textarea, Toggle } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";
import { AuthFlow } from "@/components/inspector/auth-flow";
import { CtxPanel } from "@/components/inspector/ctx-panel";
import { EndpointPanel } from "@/components/inspector/endpoint-panel";
import { TablePanel } from "@/components/inspector/table-panel";
import { ToolPanel } from "@/components/inspector/tool-panel";
import { ScriptPanel } from "@/components/inspector/script-panel";
import { McpPanel } from "@/components/inspector/mcp-panel";
import { VaultPanel } from "@/components/inspector/vault-panel";
import { ChestPanel } from "@/components/inspector/chest-panel";
import { useBoardStore } from "@/store/board";
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
  if (!node) {
    return (
      <aside
        data-testid="inspector-empty"
        className="flex w-[360px] shrink-0 flex-col border-l border-rule bg-[var(--panel-1)] p-4"
      >
        <p className="text-meta text-ink-dim">
          Pick a node to see what it is and what it may touch. Drag from a node&apos;s right edge to
          another node&apos;s left edge to wire them together.
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
      </div>

      <div className="flex flex-col gap-5 p-4">
        {node.type === "agent" ? (
          <AgentPanel node={node} api={api} onChanged={onChanged} />
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
      </div>
    </aside>
  );
}

function AgentPanel({
  node,
  api,
  onChanged,
}: {
  node: AgentNode;
  api: EngineApi;
  onChanged: () => void;
}) {
  const agent = useMemo(() => api.agent(node.id), [api, node.id]);
  const openTab = useBoardStore((s) => s.openTab);
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

      <AuthFlow api={api} nodeId={node.id} needsAuth={status === "needs_auth"} onAuthenticated={onChanged} />

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

      <Button size="sm" data-testid="btn-open-log" onClick={() => openTab(node.id)}>
        Open log and chat
      </Button>
    </>
  );
}
