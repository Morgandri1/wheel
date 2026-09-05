"use client";

import dynamic from "next/dynamic";
import { useEffect, useState } from "react";
import { Button, Field, Input, Select } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";
import { SCRIPT_LANGUAGES } from "@/lib/schema";
import type { EngineApi } from "@/lib/api";
import type { ScriptLanguage, ScriptNode } from "@/lib/schema";

import "@/lib/monaco";

const Monaco = dynamic(() => import("@monaco-editor/react"), {
  ssr: false,
  loading: () => <div className="h-[300px] animate-pulse border border-rule bg-[var(--panel-0)]" />,
});

const MONACO_LANGUAGE: Record<ScriptLanguage, string> = {
  python: "python",
  ts: "typescript",
  js: "javascript",
};

/** §3: `timeout_secs` is capped at 300 — the engine enforces it, so say so before they try. */
const MAX_TIMEOUT = 300;

export function ScriptPanel({
  node,
  api,
  onChanged,
}: {
  node: ScriptNode;
  api: EngineApi;
  onChanged: () => void;
}) {
  const [source, setSource] = useState(node.config.source);
  const [language, setLanguage] = useState<ScriptLanguage>(node.config.language);
  const [timeout, setTimeoutSecs] = useState(String(node.config.timeout_secs ?? 60));
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    setSource(node.config.source);
    setLanguage(node.config.language);
    setTimeoutSecs(String(node.config.timeout_secs ?? 60));
  }, [node.id, node.config.source, node.config.language, node.config.timeout_secs]);

  const seconds = Number(timeout);
  const timeoutError =
    !Number.isFinite(seconds) || seconds < 1
      ? "Give it at least one second."
      : seconds > MAX_TIMEOUT
        ? `The engine caps a script at ${MAX_TIMEOUT} seconds.`
        : null;

  const dirty =
    source !== node.config.source ||
    language !== node.config.language ||
    seconds !== (node.config.timeout_secs ?? 60);

  return (
    <>
      <p className="text-meta text-ink-dim">
        Agents with a read wire call this with <span className="ident">wheel run {node.name}</span>{" "}
        and get its stdout back.
      </p>

      <Field label="Language">
        <Select
          data-testid="inspector-script-language"
          value={language}
          onChange={(e) => setLanguage(e.target.value as ScriptLanguage)}
        >
          {SCRIPT_LANGUAGES.map((l) => (
            <option key={l} value={l}>
              {l === "ts" ? "TypeScript" : l === "js" ? "JavaScript" : "Python"}
            </option>
          ))}
        </Select>
      </Field>

      <Field label="Source">
        <div className="border border-rule" data-testid="inspector-script-source">
          <Monaco
            height="320px"
            language={MONACO_LANGUAGE[language]}
            theme="vs-dark"
            value={source}
            onChange={(v) => setSource(v ?? "")}
            options={{
              minimap: { enabled: false },
              fontSize: 13,
              lineNumbers: "on",
              scrollBeyondLastLine: false,
              padding: { top: 10, bottom: 10 },
              renderLineHighlight: "none",
            }}
          />
        </div>
      </Field>

      <Field label="Timeout" hint="Seconds before the engine kills the run." error={timeoutError}>
        <Input
          data-testid="inspector-script-timeout"
          inputMode="numeric"
          value={timeout}
          onChange={(e) => setTimeoutSecs(e.target.value)}
        />
      </Field>

      <div className="flex items-center justify-between gap-2">
        {/*
          Running a script from the UI needs an engine route that does not exist yet — §3 gives
          agents `wheel run`, but the control plane has no equivalent. Showing the button disabled
          with the reason is honest; hiding it would leave people hunting for it.
        */}
        <Button
          size="sm"
          disabled
          data-testid="btn-script-run"
          title="Running a script from the board arrives with the engine's run route."
        >
          Run
        </Button>
        <Button
          tone="primary"
          size="sm"
          data-testid="btn-script-save"
          disabled={!dirty || Boolean(timeoutError) || saving}
          onClick={async () => {
            setSaving(true);
            try {
              await api.patchNode(node.id, {
                config: { language, source, timeout_secs: seconds },
              });
              onChanged();
              toast("Saved.");
            } catch (e) {
              toastError(e, "Couldn't save that script.");
            } finally {
              setSaving(false);
            }
          }}
        >
          {saving ? "Saving…" : "Save"}
        </Button>
      </div>
    </>
  );
}
