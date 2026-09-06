"use client";

import { useEffect, useMemo, useState } from "react";
import { Button, CopyField, Field, Input, Select } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";
import { validateEndpointPath } from "@/lib/validate";
import { HTTP_METHODS } from "@/lib/schema";
import type { EngineApi } from "@/lib/api";
import type { EndpointNode, HttpMethod, Project, ResponseMode, WheelNode } from "@/lib/schema";

/**
 * An endpoint is the one node the outside world can touch, so the panel leads with the URL and
 * with whether that URL is actually reachable — the capability being off is the single most
 * confusing thing about a hit that 403s.
 */
export function EndpointPanel({
  node,
  nodes,
  project,
  api,
  onChanged,
}: {
  node: EndpointNode;
  nodes: WheelNode[];
  project: Project;
  api: EngineApi;
  onChanged: () => void;
}) {
  const [method, setMethod] = useState<HttpMethod>(node.config.method);
  const [path, setPath] = useState(node.config.path);
  const [responseMode, setResponseMode] = useState<ResponseMode>(node.config.response_mode);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    setMethod(node.config.method);
    setPath(node.config.path);
    setResponseMode(node.config.response_mode);
  }, [node.id, node.config.method, node.config.path, node.config.response_mode]);

  const pathError = validateEndpointPath(path);
  const dirty =
    method !== node.config.method ||
    path !== node.config.path ||
    responseMode !== node.config.response_mode;

  // `??` is wrong here: the API sends ingress_base_url as an EMPTY STRING until the project has
  // started, and an empty string is not null. That produced a "public URL" of just the path —
  // "/hook" — which looks like a URL, copies like a URL, and goes nowhere.
  const base = project.ingress_base_url || `${process.env.NEXT_PUBLIC_API_URL ?? ""}/p/${project.id}`;
  const url = `${base.replace(/\/$/, "")}${node.config.path}`;

  /** What this endpoint actually does with a hit depends entirely on where its wires go. */
  const targets = useMemo(() => {
    const byId = new Map(nodes.map((n) => [n.id, n]));
    return (node.wires ?? [])
      .map((w) => ({ wire: w, target: byId.get(w.to) }))
      .filter((x): x is { wire: (typeof x)["wire"]; target: WheelNode } => Boolean(x.target));
  }, [node.wires, nodes]);

  const save = async () => {
    setSaving(true);
    try {
      await api.patchNode(node.id, {
        config: { ...node.config, method, path, response_mode: responseMode },
      });
      onChanged();
      toast("Saved.");
    } catch (e) {
      toastError(e, "Couldn't save that endpoint.");
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      {/* Optional-chained on purpose: an older API, or any project shape that omits it, must not
          be able to blank the board. Absent capabilities read as "off", which is the safe claim —
          telling someone a URL is live when it is not costs more than the reverse. */}
      {!project.capabilities?.http ? (
        <p
          data-testid="endpoint-http-off"
          className="border border-[color-mix(in_srgb,var(--wire-write)_50%,transparent)] px-2.5 py-2 text-micro"
          style={{ color: "var(--wire-write)" }}
        >
          Public HTTP is off for this project, so this URL answers 403. Turn it on from the project
          list to make endpoints reachable.
        </p>
      ) : null}

      <Field label="Public URL" hint="Anyone with this link can hit it. There is no allowlist.">
        <CopyField value={url} testId="inspector-endpoint-url" />
      </Field>

      <div className="grid grid-cols-[96px_1fr] gap-2">
        <Field label="Method">
          <Select
            data-testid="inspector-endpoint-method"
            value={method}
            onChange={(e) => setMethod(e.target.value as HttpMethod)}
          >
            {HTTP_METHODS.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </Select>
        </Field>
        <Field label="Path" error={pathError}>
          <Input
            data-testid="inspector-endpoint-path"
            mono
            value={path}
            onChange={(e) => setPath(e.target.value)}
            placeholder="/hook"
          />
        </Field>
      </div>

      <Field
        label="Response"
        hint={
          responseMode === "ack"
            ? "Answers 202 straight away; the wired nodes do the work afterwards."
            : "Answers with the wired script's stdout. The caller waits for it."
        }
      >
        <Select
          data-testid="inspector-endpoint-response-mode"
          value={responseMode}
          onChange={(e) => setResponseMode(e.target.value as ResponseMode)}
        >
          <option value="ack">Acknowledge immediately</option>
          <option value="script">Return the script&apos;s output</option>
        </Select>
      </Field>

      <div className="flex justify-end">
        <Button
          tone="primary"
          size="sm"
          data-testid="btn-endpoint-save"
          disabled={!dirty || Boolean(pathError) || saving}
          onClick={save}
        >
          {saving ? "Saving…" : "Save"}
        </Button>
      </div>

      <div className="border-t border-rule pt-4">
        <p className="mb-2 text-micro font-medium text-ink-dim">What happens on a hit</p>
        {targets.length ? (
          <ul className="flex flex-col gap-1.5" data-testid="endpoint-targets">
            {targets.map(({ wire, target }) => (
              <li key={`${wire.to}:${wire.type}`} className="text-micro text-ink-dim">
                <span className="ident text-ink">{target.name}</span>{" "}
                {target.type === "agent"
                  ? "receives it as a message"
                  : target.type === "table"
                    ? "gets the JSON body inserted as a row"
                    : "runs with the request"}
              </li>
            ))}
          </ul>
        ) : (
          <p className="text-micro text-ink-faint">
            Nothing is wired to it yet, so a hit is accepted and dropped. Wire it to an agent, a
            table or a script.
          </p>
        )}
      </div>
    </>
  );
}
