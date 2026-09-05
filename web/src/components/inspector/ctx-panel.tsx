"use client";

import dynamic from "next/dynamic";
import { useEffect, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Button, Field } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";
import type { EngineApi } from "@/lib/api";
import type { CtxNode } from "@/lib/schema";

const Monaco = dynamic(() => import("@monaco-editor/react"), {
  ssr: false,
  loading: () => <div className="h-[260px] animate-pulse border border-rule bg-[var(--panel-0)]" />,
});

export function CtxPanel({
  node,
  api,
  onChanged,
}: {
  node: CtxNode;
  api: EngineApi;
  onChanged: () => void;
}) {
  const [markdown, setMarkdown] = useState(node.config.markdown);
  const [tab, setTab] = useState<"write" | "preview">("write");
  const [saving, setSaving] = useState(false);

  useEffect(() => setMarkdown(node.config.markdown), [node.id, node.config.markdown]);
  const dirty = markdown !== node.config.markdown;

  return (
    <>
      <p className="text-meta text-ink-dim">
        Every agent with an injection wire from this node gets this text at the top of its prompt.
      </p>

      <div className="flex items-center gap-px border border-rule">
        {(["write", "preview"] as const).map((t) => (
          <button
            key={t}
            data-testid={`ctx-tab-${t}`}
            onClick={() => setTab(t)}
            className={`flex-1 px-2 py-1 text-micro transition-colors ${
              tab === t ? "bg-[var(--panel-2)] text-ink" : "text-ink-dim hover:text-ink"
            }`}
          >
            {t === "write" ? "Write" : "Preview"}
          </button>
        ))}
      </div>

      {tab === "write" ? (
        <Field label="Markdown">
          <div className="border border-rule" data-testid="inspector-ctx-markdown">
            <Monaco
              height="300px"
              defaultLanguage="markdown"
              theme="vs-dark"
              value={markdown}
              onChange={(v) => setMarkdown(v ?? "")}
              options={{
                minimap: { enabled: false },
                fontSize: 13,
                fontFamily: "var(--font-mono), ui-monospace, monospace",
                lineNumbers: "off",
                wordWrap: "on",
                scrollBeyondLastLine: false,
                padding: { top: 10, bottom: 10 },
                renderLineHighlight: "none",
              }}
            />
          </div>
        </Field>
      ) : (
        <div
          data-testid="ctx-preview"
          className="prose-sm max-h-[320px] overflow-y-auto border border-rule p-3 text-meta [&_code]:font-mono [&_h1]:mb-2 [&_h1]:text-lead [&_h1]:font-semibold [&_h2]:mb-1.5 [&_h2]:mt-3 [&_h2]:font-semibold [&_li]:ml-4 [&_li]:list-disc [&_p]:mb-2"
        >
          {markdown.trim() ? (
            <ReactMarkdown remarkPlugins={[remarkGfm]}>{markdown}</ReactMarkdown>
          ) : (
            <p className="text-ink-faint">Nothing here yet. Whatever you write gets injected verbatim.</p>
          )}
        </div>
      )}

      <div className="flex justify-end">
        <Button
          tone="primary"
          size="sm"
          data-testid="btn-ctx-save"
          disabled={!dirty || saving}
          onClick={async () => {
            setSaving(true);
            try {
              await api.patchNode(node.id, { config: { markdown } });
              onChanged();
              toast("Saved. Agents pick it up at their next start or clear.");
            } catch (e) {
              toastError(e, "Couldn't save that context.");
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
