"use client";

import { useState } from "react";
import { Button, Dialog, Empty, Field, Input } from "@/components/ui";
import { ProposalPreview } from "@/components/builder/builder-panel";
import { capabilityWarnings } from "@/lib/workflow-proposal";
import {
  parseTemplateFile,
  templateToProposal,
  type InstantiateOutcome,
  type TemplateBoard,
  type TemplateFile,
} from "@/lib/templates";
import type { TemplateSummary } from "@/app/api/templates/route";
import type { Project } from "@/lib/schema";

/** Fetches one template's full board. A summary never carries it — see `route.ts`'s own comment. */
export type TemplateLoader = (slug: string) => Promise<{ status: "ok"; template: TemplateFile } | { status: "invalid"; problems: string[] }>;

export type TemplateInstantiator = (
  name: string,
  board: TemplateBoard,
  capabilities: { http: boolean },
) => Promise<InstantiateOutcome>;

/** The real loader: templates are plain static assets under `public/`, so no second route reads them. */
export const fetchTemplate: TemplateLoader = async (slug) => {
  const res = await fetch(`/workflow_templates/${slug}.json`, { cache: "no-store" });
  if (!res.ok) return { status: "invalid", problems: [`Couldn't load this template (${res.status}).`] };
  let raw: unknown;
  try {
    raw = await res.json();
  } catch {
    return { status: "invalid", problems: ["This template's file is not valid JSON."] };
  }
  const parsed = parseTemplateFile(raw);
  return parsed.status === "ok" ? { status: "ok", template: parsed.template } : parsed;
};

/**
 * The gallery: pick a template, preview exactly what it creates, confirm, and land on the new
 * project. Nothing exists until "Use this template" is clicked — same rule as the builder's own
 * apply step, because a template is still a board nobody has agreed to yet.
 */
export function TemplateGallery({
  summaries,
  loadTemplate,
  instantiate,
  onCreated,
}: {
  summaries: TemplateSummary[];
  loadTemplate: TemplateLoader;
  instantiate: TemplateInstantiator;
  onCreated?: (project: Project) => void;
}) {
  const [openSlug, setOpenSlug] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [template, setTemplate] = useState<TemplateFile | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [instantiating, setInstantiating] = useState(false);
  const [outcome, setOutcome] = useState<InstantiateOutcome | null>(null);

  const openTemplate = async (summary: TemplateSummary) => {
    setOpenSlug(summary.slug);
    setName(summary.title);
    setOutcome(null);
    setTemplate(null);
    setLoadError(null);
    setLoading(true);
    try {
      const r = await loadTemplate(summary.slug);
      if (r.status === "ok") setTemplate(r.template);
      else setLoadError(r.problems.join(" "));
    } finally {
      setLoading(false);
    }
  };

  const close = () => {
    setOpenSlug(null);
    setTemplate(null);
    setLoadError(null);
    setOutcome(null);
  };

  const use = async () => {
    if (!template || !name.trim()) return;
    setInstantiating(true);
    try {
      const o = await instantiate(name.trim(), template.board, template.requiresCapabilities);
      setOutcome(o);
      if (o.kind === "created") onCreated?.(o.project);
    } finally {
      setInstantiating(false);
    }
  };

  if (!summaries.length) {
    return (
      <Empty
        title="No templates yet"
        body="Templates are files dropped into public/workflow_templates — none have been added yet."
      />
    );
  }

  return (
    <>
      <ul className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3" data-testid="template-gallery">
        {summaries.map((s) => (
          <li key={s.slug}>
            <button
              type="button"
              data-testid={`template-card-${s.slug}`}
              onClick={() => openTemplate(s)}
              className="flex h-full w-full flex-col items-start gap-1.5 border border-rule p-3 text-left hover:border-[var(--wire-read)]"
            >
              <span className="text-lead font-semibold text-ink">{s.title}</span>
              <span className="text-micro text-ink-dim">{s.description}</span>
              <span className="mt-auto pt-2 text-micro text-ink-faint">
                {s.nodeCount} node{s.nodeCount === 1 ? "" : "s"}, {s.wireCount} wire{s.wireCount === 1 ? "" : "s"}
              </span>
              {s.warnings.length ? (
                <span className="text-micro text-[var(--danger)]" data-testid={`template-card-${s.slug}-warning`}>
                  {s.warnings.length} thing{s.warnings.length === 1 ? "" : "s"} in it will not run yet
                </span>
              ) : null}
            </button>
          </li>
        ))}
      </ul>

      <Dialog
        open={Boolean(openSlug)}
        onClose={close}
        title={template?.title ?? "Loading template…"}
        testId="dialog-template-preview"
      >
        {loading ? (
          <p className="text-micro text-ink-dim">Loading…</p>
        ) : loadError ? (
          <p className="text-micro text-[var(--danger)]" data-testid="template-load-error">
            {loadError}
          </p>
        ) : template ? (
          <div className="flex flex-col gap-3">
            <p className="text-micro text-ink-dim">{template.description}</p>
            <ProposalPreview
              proposal={templateToProposal(template.board)}
              warnings={capabilityWarnings(template.board.nodes)}
            />
            {template.requiresCapabilities.http ? (
              <p className="text-micro text-ink-faint" data-testid="template-needs-http">
                This template needs public HTTP — it will be turned on for the new project
                automatically.
              </p>
            ) : null}

            {outcome ? (
              <TemplateOutcome outcome={outcome} />
            ) : (
              <>
                <Field label="Project name">
                  <Input
                    data-testid="input-template-project-name"
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                    autoFocus
                  />
                </Field>
                <div className="flex justify-end gap-2">
                  <Button tone="ghost" onClick={close}>
                    Cancel
                  </Button>
                  <Button
                    tone="primary"
                    data-testid="btn-use-template"
                    disabled={!name.trim() || instantiating}
                    onClick={use}
                  >
                    {instantiating ? "Creating…" : "Use this template"}
                  </Button>
                </div>
              </>
            )}
          </div>
        ) : null}
      </Dialog>
    </>
  );
}

/**
 * Three outcomes, not four: instantiate is atomic from the user's point of view (either a real
 * project exists or nothing does), so there is no "half-built, apply again" state the way the
 * builder's own Outcome has — `readInstantiateOutcome` never reports a project that partly landed.
 */
function TemplateOutcome({ outcome }: { outcome: InstantiateOutcome }) {
  if (outcome.kind === "created") {
    return (
      <div className="border-l-2 border-[var(--live)] px-2.5 py-2" data-testid="template-outcome-created">
        <p className="text-micro text-ink">Created “{outcome.project.name}”.</p>
        <a
          href={`/app/${outcome.project.id}`}
          className="text-micro text-[var(--wire-read)] underline underline-offset-4"
          data-testid="link-go-to-project"
        >
          Go to the project
        </a>
      </div>
    );
  }

  if (outcome.kind === "refused") {
    return (
      <div className="border-l-2 border-[var(--danger)] px-2.5 py-2" data-testid="template-outcome-refused">
        <p className="text-micro text-ink">{outcome.message}</p>
        {outcome.refusals.map((r) => (
          <p key={`${r.code}-${r.message}`} className="text-micro text-ink-faint">
            {r.message}
          </p>
        ))}
        <p className="text-micro text-ink-faint">
          This template didn&apos;t pass validation — nothing was created. This shouldn&apos;t
          happen; it means something is wrong with the template itself.
        </p>
      </div>
    );
  }

  return (
    <div className="border-l-2 border-[var(--danger)] px-2.5 py-2" data-testid="template-outcome-rolled-back">
      <p className="text-micro text-ink">
        {outcome.cleanedUp
          ? "This didn't work. Nothing was created — safe to try again."
          : "This didn't work, and the partial project couldn't be cleaned up automatically."}
      </p>
      {outcome.report.failures.map((f) => (
        <p key={f.step} className="text-micro text-ink-faint" data-testid="template-outcome-failure">
          {f.wire ? `${f.wire.from} → ${f.wire.to} (${f.wire.type})` : (f.node ?? f.step)}: {f.error}
        </p>
      ))}
      {!outcome.cleanedUp && outcome.projectId ? (
        <p className="text-micro text-ink-faint" data-testid="template-outcome-orphan-id">
          Project id {outcome.projectId} — mention this if you report it.
        </p>
      ) : null}
    </div>
  );
}

