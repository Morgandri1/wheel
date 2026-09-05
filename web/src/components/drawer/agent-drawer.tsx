"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { LogStream } from "@/components/drawer/log-stream";
import { Button } from "@/components/ui";
import { toastError } from "@/components/ui/toast";
import { useBoardStore } from "@/store/board";
import { AGENT_STATUS_META } from "@/lib/node-meta";
import type { EngineApi } from "@/lib/api";
import type { WheelNode } from "@/lib/schema";

export function AgentDrawer({ nodes, api }: { nodes: WheelNode[]; api: EngineApi }) {
  const tabs = useBoardStore((s) => s.drawerTabs);
  const activeTab = useBoardStore((s) => s.activeTab);
  const open = useBoardStore((s) => s.drawerOpen);
  const setActiveTab = useBoardStore((s) => s.setActiveTab);
  const closeTab = useBoardStore((s) => s.closeTab);
  const setOpen = useBoardStore((s) => s.setDrawerOpen);
  const logs = useBoardStore((s) => s.logs);
  const messages = useBoardStore((s) => s.messages);
  const seedLog = useBoardStore((s) => s.seedLog);

  const [view, setView] = useState<"log" | "messages">("log");
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const seeded = useRef(new Set<string>());

  const byId = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes]);
  const node = activeTab ? byId.get(activeTab) : null;

  // Backfill the log once per tab; the WS carries everything after that.
  useEffect(() => {
    if (!activeTab || seeded.current.has(activeTab)) return;
    seeded.current.add(activeTab);
    void api
      .agent(activeTab)
      .log()
      .then((r) => seedLog(activeTab, r.lines))
      .catch(() => seeded.current.delete(activeTab));
  }, [activeTab, api, seedLog]);

  if (!tabs.length) return null;

  const lines = activeTab ? (logs[activeTab] ?? []) : [];
  const thread = activeTab
    ? messages.filter((m) => m.to_node === activeTab || m.from_node === activeTab)
    : [];

  const send = async () => {
    if (!activeTab || !draft.trim()) return;
    setSending(true);
    try {
      await api.agent(activeTab).send(draft.trim());
      setDraft("");
    } catch (e) {
      toastError(e, "Couldn't deliver that message.");
    } finally {
      setSending(false);
    }
  };

  return (
    <section
      data-testid="agent-drawer"
      className="flex shrink-0 flex-col border-t border-rule bg-[var(--panel-1)]"
      style={{ height: open ? 300 : 33 }}
    >
      <div className="flex h-8 shrink-0 items-stretch border-b border-rule">
        <div className="flex min-w-0 flex-1 items-stretch overflow-x-auto">
          {tabs.map((id) => {
            const n = byId.get(id);
            if (!n || n.type !== "agent") return null;
            const meta = AGENT_STATUS_META[n.state?.status ?? "stopped"];
            return (
              <div
                key={id}
                className={`flex shrink-0 items-center gap-2 border-r border-rule px-3 ${
                  id === activeTab ? "bg-[var(--panel-2)]" : ""
                }`}
              >
                <button
                  data-testid={`drawer-tab-${n.name}`}
                  onClick={() => setActiveTab(id)}
                  className="flex items-center gap-1.5 text-micro text-ink"
                >
                  <span className="h-1.5 w-1.5 rounded-full" style={{ background: meta.color }} />
                  <span className="ident">{n.name}</span>
                </button>
                <button
                  aria-label={`Close ${n.name}`}
                  onClick={() => closeTab(id)}
                  className="text-micro text-ink-faint hover:text-ink"
                >
                  ×
                </button>
              </div>
            );
          })}
        </div>

        <div className="flex items-center gap-1 px-2">
          {(["log", "messages"] as const).map((v) => (
            <button
              key={v}
              data-testid={`drawer-view-${v}`}
              onClick={() => setView(v)}
              className={`px-2 py-0.5 text-micro transition-colors ${
                view === v ? "text-ink" : "text-ink-faint hover:text-ink-dim"
              }`}
            >
              {v === "log" ? "Log" : "Messages"}
            </button>
          ))}
          <button
            data-testid="btn-drawer-toggle"
            onClick={() => setOpen(!open)}
            className="px-2 py-0.5 text-micro text-ink-faint hover:text-ink"
          >
            {open ? "Hide" : "Show"}
          </button>
        </div>
      </div>

      {open ? (
        <>
          <div className="min-h-0 flex-1">
            {view === "log" ? (
              <LogStream lines={lines} />
            ) : (
              <ul className="h-full overflow-y-auto p-3" data-testid="message-list">
                {thread.length ? (
                  thread.map((m) => (
                    <li key={m.id} className="mb-2.5 border-l-2 border-rule pl-2.5">
                      <p className="text-micro text-ink-faint">
                        <span className="ident text-ink-dim">{m.from_name ?? m.from_node}</span>
                        {m.from_type ? ` · ${m.from_type}` : ""} ·{" "}
                        {new Date(m.created_at).toLocaleTimeString()}
                        {m.delivered_at ? "" : " · queued"}
                      </p>
                      <p className="whitespace-pre-wrap text-meta">{m.body}</p>
                    </li>
                  ))
                ) : (
                  <li className="text-micro text-ink-faint">
                    No messages yet. Anything you send below shows up here, alongside messages from
                    other nodes.
                  </li>
                )}
              </ul>
            )}
          </div>

          <div className="flex shrink-0 items-end gap-2 border-t border-rule p-2">
            <textarea
              data-testid="chat-input"
              rows={1}
              value={draft}
              placeholder={node ? `Message ${node.name}…` : "Pick an agent"}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  void send();
                }
              }}
              className="max-h-24 min-h-[32px] flex-1 resize-none rounded-control border border-rule bg-[var(--panel-0)] px-2.5 py-1.5 text-meta text-ink placeholder:text-ink-faint focus:border-[var(--wire-read)] focus:outline-none"
            />
            <Button
              tone="primary"
              data-testid="chat-send"
              disabled={!draft.trim() || sending || !activeTab}
              onClick={send}
            >
              {sending ? "Sending…" : "Send"}
            </Button>
          </div>
        </>
      ) : null}
    </section>
  );
}
