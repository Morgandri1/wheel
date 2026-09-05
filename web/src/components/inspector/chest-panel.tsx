"use client";

import { useQuery } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { Button, Field, Input } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";
import { LIMITS, formatBytes } from "@/lib/limits";
import { validateChestKey } from "@/lib/validate";
import type { EngineApi } from "@/lib/api";
import type { ChestNode } from "@/lib/schema";

/** A file browser over the chest: list, upload, download. Keys are relative paths. */
export function ChestPanel({
  node,
  api,
  projectId,
}: {
  node: ChestNode;
  api: EngineApi;
  projectId: string;
}) {
  const chest = api.chest(node.id);
  const [prefix, setPrefix] = useState("");
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);
  const [file, setFile] = useState<File | null>(null);

  const listing = useQuery({
    queryKey: ["chest", projectId, node.id, prefix],
    queryFn: () => chest.ls(prefix),
  });

  const keyError = key ? validateChestKey(key) : null;
  const tooBig = file && file.size > LIMITS.blobBytes;

  const upload = async () => {
    if (!file || !key || keyError || tooBig) return;
    setBusy(true);
    try {
      await chest.put(key, file);
      setKey("");
      setFile(null);
      if (fileRef.current) fileRef.current.value = "";
      await listing.refetch();
      toast(`Uploaded ${key}.`);
    } catch (e) {
      toastError(e, "Couldn't upload that file.");
    } finally {
      setBusy(false);
    }
  };

  const download = async (entryKey: string) => {
    try {
      const blob = await chest.get(entryKey);
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = entryKey.split("/").pop() ?? entryKey;
      a.click();
      URL.revokeObjectURL(url);
    } catch (e) {
      toastError(e, "Couldn't download that file.");
    }
  };

  return (
    <>
      <p className="text-meta text-ink-dim">
        Files, addressed by relative path. Agents wired <span className="ident">read</span> can
        list and fetch; <span className="ident">write</span> also lets them put and remove.
      </p>

      <Field label="Filter" hint="Only paths starting with this prefix.">
        <Input
          mono
          data-testid="chest-prefix"
          value={prefix}
          placeholder="reports/"
          onChange={(e) => setPrefix(e.target.value)}
        />
      </Field>

      {listing.isPending ? (
        <p className="text-micro text-ink-faint">Loading…</p>
      ) : listing.error ? (
        <p className="text-micro text-[var(--danger)]">{(listing.error as Error).message}</p>
      ) : !listing.data?.entries.length ? (
        <p className="text-micro text-ink-faint" data-testid="chest-empty">
          {prefix ? "Nothing under that prefix." : "Empty. Upload a file, or let an agent write one."}
        </p>
      ) : (
        <ul className="border border-rule" data-testid="chest-entries">
          {listing.data.entries.map((e) => (
            <li
              key={e.key}
              data-testid={`chest-entry-${e.key}`}
              className="flex items-center gap-2 border-b border-rule px-2.5 py-1.5 last:border-b-0"
            >
              <span className="ident min-w-0 flex-1 truncate text-micro text-ink">{e.key}</span>
              <span className="shrink-0 text-micro text-ink-faint">{formatBytes(e.bytes)}</span>
              <Button
                size="sm"
                tone="ghost"
                data-testid={`btn-chest-get-${e.key}`}
                onClick={() => download(e.key)}
              >
                Download
              </Button>
            </li>
          ))}
        </ul>
      )}

      <div className="flex flex-col gap-3 border-t border-rule pt-4">
        <Field
          label="Path"
          error={keyError}
          hint="Relative, no leading slash. An existing path is overwritten."
        >
          <Input
            mono
            data-testid="chest-upload-key"
            value={key}
            placeholder="reports/2026-09.pdf"
            onChange={(e) => setKey(e.target.value)}
          />
        </Field>
        <Field
          label="File"
          error={tooBig ? `That is over the ${formatBytes(LIMITS.blobBytes)} limit.` : null}
        >
          <input
            ref={fileRef}
            type="file"
            data-testid="chest-upload-file"
            className="w-full text-micro text-ink-dim file:mr-2 file:rounded-control file:border file:border-rule file:bg-[var(--panel-2)] file:px-2 file:py-1 file:text-micro file:text-ink"
            onChange={(e) => {
              const f = e.target.files?.[0] ?? null;
              setFile(f);
              if (f && !key) setKey(f.name);
            }}
          />
        </Field>
        <div className="flex justify-end">
          <Button
            tone="primary"
            size="sm"
            data-testid="btn-chest-upload"
            disabled={!file || !key || Boolean(keyError) || Boolean(tooBig) || busy}
            onClick={upload}
          >
            {busy ? "Uploading…" : "Upload"}
          </Button>
        </div>
      </div>
    </>
  );
}
