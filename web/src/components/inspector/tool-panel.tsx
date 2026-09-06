"use client";

import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Button, Field, Input, Select, Textarea } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";
import type { EngineApi } from "@/lib/api";
import type { Fill, FillMode, ToolNode, ToolOperation, ToolParam, WheelNode } from "@/lib/schema";

/**
 * §3d. The thing this panel exists to make obvious is WHO SUPPLIES WHAT: every header, path,
 * query and cookie field is either the agent's to fill or the operator's to pin, and a secret
 * pinned here is never shown to the agent, never returned by the board, and never rendered into
 * the copyable curl. The "what the agent sees" preview is the proof, read back from the engine.
 */
export function ToolPanel({
  node,
  nodes,
  api,
  projectId,
  onChanged,
}: {
  node: ToolNode;
  nodes: WheelNode[];
  api: EngineApi;
  projectId: string;
  onChanged: () => void;
}) {
  const [tab, setTab] = useState<"operations" | "import" | "agent" | "test">("operations");
  const operations = useMemo(() => node.config?.operations ?? [], [node.config?.operations]);

  return (
    <>
      <p className="text-meta text-ink-dim">
        {node.config?.base_url ? (
          <>
            Calls go to <span className="ident">{node.config.base_url}</span>.
          </>
        ) : (
          "Import a spec to give this tool its operations."
        )}
      </p>

      <div className="flex items-center gap-px border border-rule">
        {(["operations", "import", "agent", "test"] as const).map((t) => (
          <button
            key={t}
            data-testid={`tool-tab-${t}`}
            onClick={() => setTab(t)}
            className={`flex-1 px-1.5 py-1 text-micro capitalize transition-colors ${
              tab === t ? "bg-[var(--panel-2)] text-ink" : "text-ink-dim hover:text-ink"
            }`}
          >
            {t === "agent" ? "Agent view" : t}
          </button>
        ))}
      </div>

      {tab === "import" ? (
        <ImportPanel node={node} api={api} onChanged={onChanged} />
      ) : tab === "agent" ? (
        <AgentView node={node} api={api} projectId={projectId} />
      ) : tab === "test" ? (
        <TestPanel node={node} operations={operations} api={api} />
      ) : (
        <Operations node={node} nodes={nodes} operations={operations} api={api} onChanged={onChanged} />
      )}
    </>
  );
}

// ── operations + the per-field fill editor ──────────────────────────────────

function Operations({
  node,
  nodes,
  operations,
  api,
  onChanged,
}: {
  node: ToolNode;
  nodes: WheelNode[];
  operations: ToolOperation[];
  api: EngineApi;
  onChanged: () => void;
}) {
  const [open, setOpen] = useState<string | null>(null);

  /** §3d: a vault fill needs a tool → vault read wire, so only wired vaults can be picked. */
  const wiredVaults = useMemo(() => {
    const byId = new Map(nodes.map((n) => [n.id, n]));
    return (node.wires ?? [])
      .filter((w) => w.type === "read")
      .map((w) => byId.get(w.to))
      .filter((n): n is Extract<WheelNode, { type: "vault" }> => n?.type === "vault");
  }, [node.wires, nodes]);

  const save = async (next: ToolOperation[]) => {
    try {
      await api.patchNode(node.id, { config: { ...node.config, operations: next } });
      onChanged();
    } catch (e) {
      toastError(e, "Couldn't save that change.");
    }
  };

  const patchOp = (id: string, patch: Partial<ToolOperation>) =>
    save(operations.map((o) => (o.id === id ? { ...o, ...patch } : o)));

  if (!operations.length) {
    return (
      <p className="text-micro text-ink-faint" data-testid="tool-no-operations">
        No operations yet. Import an OpenAPI or Swagger document on the Import tab.
      </p>
    );
  }

  return (
    <div className="flex flex-col gap-1.5" data-testid="tool-operations">
      {operations.map((op) => {
        const params = op.params ?? [];
        const agentCount = params.filter((p) => (p.fill?.mode ?? "agent") === "agent").length;
        const isOpen = open === op.id;
        return (
          <div key={op.id} className="border border-rule" data-testid={`tool-op-${op.id}`}>
            <div className="flex items-start gap-2 px-2 py-1.5">
              <input
                type="checkbox"
                aria-label={`Enable ${op.id}`}
                data-testid={`tool-op-${op.id}-enabled`}
                checked={op.enabled !== false}
                onChange={(e) => patchOp(op.id, { enabled: e.target.checked })}
                className="mt-1"
              />
              <button className="min-w-0 flex-1 text-left" onClick={() => setOpen(isOpen ? null : op.id)}>
                <span className="ident text-micro text-ink">
                  <span style={{ color: "var(--wire-write)" }}>{op.method}</span> {op.path}
                </span>
                {op.summary ? <p className="text-micro text-ink-dim">{op.summary}</p> : null}
                <p className="text-micro text-ink-faint">
                  {params.length === 0
                    ? "no fields"
                    : `${agentCount} of ${params.length} field${params.length === 1 ? "" : "s"} left to the agent`}
                </p>
              </button>
            </div>

            {isOpen && params.length ? (
              <div className="border-t border-rule px-2 py-2">
                {params.map((param, i) => (
                  <FillEditor
                    key={`${param.location}:${param.name}`}
                    param={param}
                    vaults={wiredVaults}
                    testId={`tool-op-${op.id}-field-${i}`}
                    onChange={(fill) =>
                      patchOp(op.id, {
                        params: params.map((p2, j) => (i === j ? { ...p2, fill } : p2)),
                      })
                    }
                  />
                ))}
              </div>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}

const FILL_HINT: Record<FillMode, string> = {
  agent: "The agent supplies it. It appears in the tool's input schema.",
  static: "A fixed value you set. Never shown to the agent.",
  vault: "Resolved from the vault at call time. Never shown to the agent or returned by the board.",
  hidden: "Left out of the request entirely.",
};

function FillEditor({
  param,
  vaults,
  testId,
  onChange,
}: {
  param: ToolParam;
  vaults: Extract<WheelNode, { type: "vault" }>[];
  testId: string;
  onChange: (fill: Fill) => void;
}) {
  const mode: FillMode = param.fill?.mode ?? "agent";

  return (
    <div className="mb-2 border-b border-rule pb-2 last:mb-0 last:border-b-0 last:pb-0">
      <div className="flex items-center gap-2">
        <span className="ident min-w-0 flex-1 truncate text-micro text-ink">
          {param.name}
          <span className="text-ink-faint"> · {param.location}</span>
          {param.required ? <span style={{ color: "var(--danger)" }}> *</span> : null}
        </span>
        <Select
          data-testid={`${testId}-mode`}
          aria-label={`How ${param.name} is filled`}
          className="w-[104px]"
          value={mode}
          onChange={(e) => {
            const next = e.target.value as FillMode;
            onChange(
              next === "vault"
                ? { mode: next, vault_ref: param.fill?.vault_ref ?? "" }
                : next === "static"
                  ? { mode: next, value: param.fill?.value ?? "" }
                  : { mode: next },
            );
          }}
        >
          <option value="agent">agent</option>
          <option value="static">static</option>
          <option value="vault" disabled={vaults.length === 0}>
            vault
          </option>
          <option value="hidden">hidden</option>
        </Select>
      </div>

      <p className="mt-1 text-micro text-ink-faint">
        {FILL_HINT[mode]}
        {vaults.length === 0 && mode !== "vault"
          ? " Wire this tool to a vault node to fill it from a secret."
          : ""}
      </p>

      {/* §3d says `hidden` omits the field. A path parameter is not a field — it is a hole in the
          URL, and omitting it builds a different URL rather than a smaller request. The engine is
          the authority, so this warns instead of refusing; a greyed option with no reason is the
          thing we just removed. */}
      {mode === "hidden" && param.location === "path" ? (
        <p className="mt-1 text-micro" style={{ color: "var(--danger)" }} data-testid={`${testId}-hidden-path`}>
          Hiding a path parameter does not omit a field, it changes the URL — {param.name} is part
          of the path itself. Pin it with static or vault instead.
        </p>
      ) : null}

      {mode === "static" ? (
        <Input
          className="mt-1.5"
          mono
          data-testid={`${testId}-value`}
          aria-label={`Value for ${param.name}`}
          value={param.fill?.value ?? ""}
          onChange={(e) => onChange({ mode: "static", value: e.target.value })}
        />
      ) : mode === "vault" ? (
        vaults.length ? (
          <Input
            className="mt-1.5"
            mono
            data-testid={`${testId}-vault-ref`}
            aria-label={`Vault reference for ${param.name}`}
            list={`${testId}-vaults`}
            placeholder={`${vaults[0]!.name}/key-name`}
            value={param.fill?.vault_ref ?? ""}
            onChange={(e) => onChange({ mode: "vault", vault_ref: e.target.value })}
          />
        ) : null
      ) : null}

      {mode === "vault" ? (
        <datalist id={`${testId}-vaults`}>
          {vaults.flatMap((v) =>
            (v.config.keys ?? []).map((k) => <option key={`${v.name}/${k}`} value={`${v.name}/${k}`} />),
          )}
        </datalist>
      ) : null}

    </div>
  );
}

// ── import ──────────────────────────────────────────────────────────────────

function ImportPanel({
  node,
  api,
  onChanged,
}: {
  node: ToolNode;
  api: EngineApi;
  onChanged: () => void;
}) {
  const [raw, setRaw] = useState("");
  const [busy, setBusy] = useState(false);
  const [diff, setDiff] = useState<{ added: string[]; removed: string[]; kept: string[] } | null>(null);

  const hasOps = (node.config?.operations ?? []).length > 0;

  return (
    <div className="flex flex-col gap-2" data-testid="tool-import">
      <Field
        label="OpenAPI 3 or Swagger 2 document"
        hint={
          hasOps
            ? "Re-importing keeps the fills you have already set and shows what changed."
            : "Paste the document, or drop a file onto this box."
        }
      >
        <Textarea
          mono
          rows={8}
          data-testid="tool-import-raw"
          value={raw}
          placeholder='{ "openapi": "3.0.0", … }'
          onChange={(e) => setRaw(e.target.value)}
          onDrop={async (e) => {
            e.preventDefault();
            const file = e.dataTransfer.files[0];
            if (file) setRaw(await file.text());
          }}
        />
      </Field>

      <div className="flex justify-end gap-2">
        <Button
          size="sm"
          data-testid="btn-tool-import"
          disabled={!raw.trim() || busy}
          onClick={async () => {
            setBusy(true);
            setDiff(null);
            try {
              const result = await api.tools.reimport(node.id, raw);
              setDiff(result);
              onChanged();
              toast(`Imported ${result.operations.length} operations.`);
            } catch (e) {
              toastError(e, "Couldn't read that document.");
            } finally {
              setBusy(false);
            }
          }}
        >
          {busy ? "Reading…" : hasOps ? "Re-import" : "Import"}
        </Button>
      </div>

      {diff ? (
        <div className="border border-rule p-2 text-micro" data-testid="tool-import-diff">
          <DiffList label="Added" items={diff.added} tone="var(--live)" />
          <DiffList label="Removed" items={diff.removed} tone="var(--danger)" />
          <DiffList label="Unchanged, fills kept" items={diff.kept} tone="var(--ink-faint)" />
        </div>
      ) : null}
    </div>
  );
}

function DiffList({ label, items, tone }: { label: string; items: string[]; tone: string }) {
  if (!items.length) return null;
  return (
    <div className="mb-1.5 last:mb-0">
      <p style={{ color: tone }}>
        {label} ({items.length})
      </p>
      <ul className="ident text-ink-dim">
        {items.slice(0, 12).map((i) => (
          <li key={i}>{i}</li>
        ))}
        {items.length > 12 ? <li className="text-ink-faint">…and {items.length - 12} more</li> : null}
      </ul>
    </div>
  );
}

// ── what the agent sees ─────────────────────────────────────────────────────

function AgentView({ node, api, projectId }: { node: ToolNode; api: EngineApi; projectId: string }) {
  const ops = useQuery({
    queryKey: ["tool-ops", projectId, node.id, node.config?.operations],
    queryFn: () => api.tools.ops(node.id),
  });

  return (
    <div data-testid="tool-agent-view">
      <p className="mb-2 text-micro text-ink-dim">
        Read back from the engine — this is exactly what an agent wired to this tool can see and
        supply. Anything you pinned to a static value or a vault is absent.
      </p>
      {ops.isPending ? (
        <p className="text-micro text-ink-faint">Loading…</p>
      ) : ops.error ? (
        <p className="text-micro text-[var(--danger)]">{(ops.error as Error).message}</p>
      ) : !ops.data?.operations.length ? (
        <p className="text-micro text-ink-faint">
          Nothing is enabled, so an agent sees no operations at all.
        </p>
      ) : (
        <ul className="flex flex-col gap-2">
          {ops.data.operations.map((op) => (
            <li key={op.id} className="border border-rule p-2" data-testid={`tool-agent-op-${op.id}`}>
              {/* The engine's own name, not one assembled here — see api.ts. The fallback covers
                  an engine too old to send it and is not the expected path. */}
              <p className="ident text-micro text-ink">{op.name ?? `${node.name}__${op.id}`}</p>
              {op.description ?? op.summary ? (
                <p className="text-micro text-ink-dim">{op.description ?? op.summary}</p>
              ) : null}
              <pre className="ident mt-1 overflow-x-auto text-micro text-ink-faint">
                {JSON.stringify(op.input_schema, null, 2)}
              </pre>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

// ── test call ───────────────────────────────────────────────────────────────

function TestPanel({
  node,
  operations,
  api,
}: {
  node: ToolNode;
  operations: ToolOperation[];
  api: EngineApi;
}) {
  const enabled = operations.filter((o) => o.enabled !== false);
  const [opId, setOpId] = useState(enabled[0]?.id ?? "");
  const [args, setArgs] = useState("{}");
  const [result, setResult] = useState<string | null>(null);
  const [curl, setCurl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const run = async (dryRun: boolean) => {
    setError(null);
    setResult(null);
    setCurl(null);
    let parsed: Record<string, unknown>;
    try {
      parsed = JSON.parse(args || "{}") as Record<string, unknown>;
    } catch {
      setError("Those arguments are not valid JSON.");
      return;
    }
    try {
      const r = await api.tools.call(node.id, opId, parsed, dryRun);
      if (r.curl) setCurl(r.curl);
      else setResult(JSON.stringify({ status: r.status, headers: r.headers, body: r.body }, null, 2));
    } catch (e) {
      setError((e as Error).message);
    }
  };

  if (!enabled.length) {
    return <p className="text-micro text-ink-faint">Enable an operation before testing it.</p>;
  }

  return (
    <div className="flex flex-col gap-2" data-testid="tool-test">
      <Field label="Operation">
        <Select data-testid="tool-test-op" value={opId} onChange={(e) => setOpId(e.target.value)}>
          {enabled.map((o) => (
            <option key={o.id} value={o.id}>
              {o.method} {o.path}
            </option>
          ))}
        </Select>
      </Field>

      <Field label="Arguments" hint="Only the fields left to the agent. Anything else is refused.">
        <Textarea mono rows={4} data-testid="tool-test-args" value={args} onChange={(e) => setArgs(e.target.value)} />
      </Field>

      <div className="flex justify-end gap-2">
        <Button size="sm" data-testid="btn-tool-curl" onClick={() => run(true)}>
          Copy as curl
        </Button>
        <Button size="sm" tone="primary" data-testid="btn-tool-call" onClick={() => run(false)}>
          Send
        </Button>
      </div>

      {error ? (
        <p className="text-micro text-[var(--danger)]" data-testid="tool-test-error">
          {error}
        </p>
      ) : null}

      {curl ? (
        <div>
          <p className="mb-1 text-micro text-ink-faint">
            Static and vault values are masked — a secret is never rendered, not even into
            something you asked to copy.
          </p>
          <pre
            data-testid="tool-test-curl"
            className="ident overflow-x-auto border border-rule bg-[var(--panel-0)] p-2 text-micro"
          >
            {curl}
          </pre>
          <div className="mt-1 flex justify-end">
            <Button size="sm" onClick={() => navigator.clipboard.writeText(curl)}>
              Copy
            </Button>
          </div>
        </div>
      ) : null}

      {result ? (
        <pre
          data-testid="tool-test-result"
          className="ident max-h-56 overflow-auto border border-rule bg-[var(--panel-0)] p-2 text-micro"
        >
          {result}
        </pre>
      ) : null}
    </div>
  );
}
