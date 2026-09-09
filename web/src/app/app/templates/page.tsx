"use client";

import { useRouter } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import { Header } from "@/components/header";
import { Button, Empty, Skeleton } from "@/components/ui";
import { fetchTemplate, TemplateGallery } from "@/components/templates/template-gallery";
import { instantiateTemplate } from "@/lib/api";
import type { TemplateSummary } from "@/app/api/templates/route";

async function listTemplates(): Promise<TemplateSummary[]> {
  const res = await fetch("/api/templates", { cache: "no-store" });
  if (!res.ok) throw new Error(`Couldn't load the template gallery (${res.status}).`);
  const body = (await res.json()) as { templates: TemplateSummary[] };
  return body.templates;
}

export default function TemplatesPage() {
  const router = useRouter();
  const { data, isPending, error, refetch } = useQuery({
    queryKey: ["templates"],
    queryFn: listTemplates,
  });

  return (
    <div className="flex min-h-screen flex-col">
      <Header />
      <main className="mx-auto w-full max-w-5xl flex-1 px-6 py-10">
        <div className="mb-8">
          <h1 className="display text-h2">Templates</h1>
          <p className="mt-1 max-w-[62ch] text-meta text-ink-dim">
            Pick a starting board, name your project, and edit from there. Nothing exists until you
            confirm.
          </p>
        </div>

        {isPending ? (
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {[0, 1, 2].map((i) => (
              <Skeleton key={i} className="h-[104px] w-full" />
            ))}
          </div>
        ) : error ? (
          <Empty
            title="Can't load templates"
            body={(error as Error).message}
            action={<Button onClick={() => refetch()}>Try again</Button>}
          />
        ) : (
          <TemplateGallery
            summaries={data ?? []}
            loadTemplate={fetchTemplate}
            instantiate={instantiateTemplate}
            onCreated={(project) => router.push(`/app/${project.id}`)}
          />
        )}
      </main>
    </div>
  );
}
