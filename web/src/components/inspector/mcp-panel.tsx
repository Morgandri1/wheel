"use client";

import { useEffect, useState } from "react";
import { Button, Field, Input, Select } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";
import type { EngineApi } from "@/lib/api";
import type { McpNode } from "@/lib/schema";

type Transport = "stdio" | "http";

/**
 * The engine models MCP config as a tagged union — stdio requires a command, http requires a URL,
 * never both — so this panel edits one shape or the other rather than a bag of optionals. Switching
 * transport discards the other side's fields, which is what the engine would do anyway.
 */
export function McpPanel({
  node,
  api,
  onChanged,
}: {
  node: McpNode;
  api: EngineApi;
  onChanged: () => void;
}) {
  const config = node.config;
  const [transport, setTransport] = useState<Transport>(config.transport);
  const [command, setCommand] = useState(config.transport === "stdio" ? config.command : "");
  const [args, setArgs] = useState(
    config.transport === "stdio" ? (config.args ?? []).join(" ") : "",
  );
  const [url, setUrl] = useState(config.transport === "http" ? config.url : "");
  const [env, setEnv] = useState(
    Object.entries(config.env ?? {})
      .map(([k, v]) => `${k}=${v}`)
      .join("\n"),
  );
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    setTransport(config.transport);
    setCommand(config.transport === "stdio" ? config.command : "");
    setArgs(config.transport === "stdio" ? (config.args ?? []).join(" ") : "");
    setUrl(config.transport === "http" ? config.url : "");
    setEnv(
      Object.entries(config.env ?? {})
        .map(([k, v]) => `${k}=${v}`)
        .join("\n"),
    );
  }, [node.id, config]);

  const urlError =
    transport === "http" && url && !/^https?:\/\/\S+$/.test(url)
      ? "Give it an absolute http:// or https:// URL."
      : null;

  const parsedEnv = Object.fromEntries(
    env
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean)
      .map((line) => {
        const at = line.indexOf("=");
        return at === -1 ? [line, ""] : [line.slice(0, at).trim(), line.slice(at + 1).trim()];
      }),
  );

  const incomplete = transport === "stdio" ? !command.trim() : !url.trim();

  const save = async () => {
    setSaving(true);
    try {
      const next =
        transport === "stdio"
          ? {
              transport: "stdio" as const,
              command: command.trim(),
              args: args.trim() ? args.trim().split(/\s+/) : [],
              env: parsedEnv,
            }
          : { transport: "http" as const, url: url.trim(), env: parsedEnv };
      await api.patchNode(node.id, { config: next });
      onChanged();
      toast("Saved. Wired agents pick it up at their next start.");
    } catch (e) {
      toastError(e, "Couldn't save that server.");
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <p className="text-meta text-ink-dim">
        Attached to the harness of every agent wired to it, at that agent&apos;s next start — not
        to the one running now.
      </p>

      <Field label="Transport">
        <Select
          data-testid="inspector-mcp-transport"
          value={transport}
          onChange={(e) => setTransport(e.target.value as Transport)}
        >
          <option value="stdio">stdio — a local process</option>
          <option value="http">http — a remote server</option>
        </Select>
      </Field>

      {transport === "stdio" ? (
        <>
          <Field label="Command">
            <Input
              mono
              data-testid="inspector-mcp-command"
              value={command}
              placeholder="npx"
              onChange={(e) => setCommand(e.target.value)}
            />
          </Field>
          <Field label="Arguments" hint="Space-separated.">
            <Input
              mono
              data-testid="inspector-mcp-args"
              value={args}
              placeholder="-y @modelcontextprotocol/server-filesystem /data"
              onChange={(e) => setArgs(e.target.value)}
            />
          </Field>
        </>
      ) : (
        <Field label="URL" error={urlError}>
          <Input
            mono
            data-testid="inspector-mcp-url"
            value={url}
            placeholder="https://mcp.example.com/sse"
            onChange={(e) => setUrl(e.target.value)}
          />
        </Field>
      )}

      <Field
        label="Environment"
        hint="One KEY=value per line. For secrets, wire a vault to the agent instead — these are stored in the clear."
      >
        <textarea
          data-testid="inspector-mcp-env"
          rows={3}
          value={env}
          onChange={(e) => setEnv(e.target.value)}
          className="ident w-full resize-y rounded-control border border-rule bg-[var(--panel-0)] px-2.5 py-1.5 text-meta text-ink placeholder:text-ink-faint focus:border-[var(--wire-read)] focus:outline-none"
          placeholder="LOG_LEVEL=debug"
        />
      </Field>

      <div className="flex justify-end">
        <Button
          tone="primary"
          size="sm"
          data-testid="btn-mcp-save"
          disabled={incomplete || Boolean(urlError) || saving}
          onClick={save}
        >
          {saving ? "Saving…" : "Save"}
        </Button>
      </div>
    </>
  );
}
