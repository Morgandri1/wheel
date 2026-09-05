"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { LogStream } from "@/components/drawer/log-stream";
import { Button } from "@/components/ui";
import { toastError } from "@/components/ui/toast";
import { useBoardStore } from "@/store/board";
import { AGENT_STATUS_META } from "@/lib/node-meta";
import { clearDraft, readDraft, writeDraft } from "@/lib/drafts";
import { displayState, senderKind, senderLabel } from "@/lib/message-state";
import { LIMITS, byteLength, checkLimit, formatBytes } from "@/lib/limits";
import type { EngineApi } from "@/lib/api";
import type { AgentStatus, Message, WheelNode } from "@/lib/schema";

export function AgentDrawer({
  nodes,
  api,
  projectId,
}: {
  nodes: WheelNode[];
  api: EngineApi;
  projectId: string;
}) {
  const tabs = useBoardStore((s) => s.drawerTabs);
  const activeTab = useBoardStore((s) => s.activeTab);
  const open = useBoardStore((s) => s.drawerOpen);
  const setActiveTab = useBoardStore((s) => s.setActiveTab);
  const closeTab = useBoardStore((s) => s.closeTab);
  const setOpen = useBoardStore((s) => s.setDrawerOpen);
  const logs = useBoardStore((s) => s.logs);
  const messages = useBoardStore((s) => s.messages);
  const seedLog = useBoardStore((s) => s.seedLog);

  const [view, setView] = useState<"log" | "transcript" | "messages">("log");
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const seeded = useRef(new Set<string>());

  const byId = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes]);
  const node = activeTab ? byId.get(activeTab) : null;

  // §3c #12: the chat box is a draft until Send, kept per agent so a reload never eats it.
  useEffect(() => {
    setDraft(activeTab ? readDraft(projectId, activeTab) : "");
  }, [activeTab, projectId]);

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

  const all = activeTab ? (logs[activeTab] ?? []) : [];
  /**
   * §3c #10. The transcript is the exact bytes written to the child's stdin, carried on the same
   * log stream — so this is a filter, not a second subscription. Splitting it out matters because
   * the operator's question is "what did the agent actually receive?", and the answer is drowned
   * in stdout when the two are interleaved.
   */
  const lines = view === "transcript" ? all.filter((l) => l.stream === "transcript") : all;
  const thread = activeTab
    ? messages.filter((m) => m.to === activeTab || (m.from.kind === "node" && m.from.id === activeTab))
    : [];

  const body = draft.trim();
  const bytes = byteLength(body);
  // §3c #6: refuse before sending, and say how much to trim, rather than failing downstream.
  const overLimit = checkLimit("messageBytes", bytes);

  const send = async () => {
    if (!activeTab || !body || overLimit) return;
    setSending(true);
    try {
      await api.agent(activeTab).send(body);
      // The row is created queued; the WS `message` event drives it to delivered and consumed.
      setDraft("");
      clearDraft(projectId, activeTab);
    } catch (e) {
      // Keep the draft: it is the person's text, and the send did not happen.
      toastError(e, "Couldn't queue that message.");
    } finally {
      setSending(false);
    }
  };

  const onDraftChange = (value: string) => {
    setDraft(value);
    if (activeTab) writeDraft(projectId, activeTab, value);
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
          {(["log", "transcript", "messages"] as const).map((v) => (
            <button
              key={v}
              data-testid={`drawer-view-${v}`}
              onClick={() => setView(v)}
              title={
                v === "transcript"
                  ? "The exact bytes written to this agent's stdin"
                  : undefined
              }
              className={`px-2 py-0.5 text-micro capitalize transition-colors ${
                view === v ? "text-ink" : "text-ink-faint hover:text-ink-dim"
              }`}
            >
              {v}
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
            {view === "log" || view === "transcript" ? (
              <LogStream lines={lines} empty={view === "transcript" ? TRANSCRIPT_EMPTY : undefined} />
            ) : (
              <ul className="h-full overflow-y-auto p-3" data-testid="message-list">
                {thread.length ? (
                  thread.map((m) => (
                    <li
                      key={m.id}
                      data-testid={`msg-${m.id}`}
                      className="mb-2.5 border-l-2 border-rule pl-2.5"
                    >
                      <p className="flex flex-wrap items-center gap-x-1.5 text-micro text-ink-faint">
                        <span className="ident text-ink-dim">{senderLabel(m.from)}</span>
                        <span>{senderKind(m.from)}</span>
                        <span>{new Date(m.created_at).toLocaleTimeString()}</span>
                        <MessageStatePill
                          message={m}
                          messages={thread}
                          agentId={activeTab ?? ""}
                          agentStatus={node?.state?.status}
                        />
                        <span
                          className="ident text-ink-faint"
                          data-testid={`msg-${m.id}-sha`}
                          title={`sha256 ${m.sha256} · ${m.bytes} bytes as sent`}
                        >
                          {m.sha256.slice(0, 8)}
                        </span>
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

          <div className="shrink-0 border-t border-rule p-2">
            {overLimit ? (
              <p className="mb-1.5 text-micro text-[var(--danger)]" data-testid="chat-limit-error">
                {overLimit}
              </p>
            ) : bytes > LIMITS.messageBytes * 0.8 ? (
              <p className="mb-1.5 text-micro text-ink-dim" data-testid="chat-limit-warning">
                {formatBytes(bytes)} of {formatBytes(LIMITS.messageBytes)}
              </p>
            ) : null}
            <div className="flex items-end gap-2">
            <textarea
              data-testid="chat-input"
              rows={1}
              value={draft}
              placeholder={node ? `Message ${node.name}…` : "Pick an agent"}
              onChange={(e) => onDraftChange(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  void send();
                }
              }}
              className="max-h-24 min-h-[32px] flex-1 resize-none rounded-control border border-rule bg-[var(--panel-0)] px-2.5 py-1.5 text-meta text-ink placeholder:text-ink-faint focus:border-[var(--wire-read)] focus:outline-none"
            />
            {/*
              §3c #12: interrupting is a deliberate, separate act — it cancels the turn the agent
              is in the middle of. It is never what Send does. The button is present but inert
              until the engine exposes POST /v1/agents/:id/interrupt in M2, so the shape of the
              interaction is visible now and cannot be confused with sending.
            */}
            <Button
              data-testid="chat-interrupt"
              disabled
              title="Interrupting a running turn arrives with the engine's interrupt route (M2)."
            >
              Interrupt
            </Button>
            <Button
              tone="primary"
              data-testid="chat-send"
              disabled={!body || sending || !activeTab || Boolean(overLimit)}
              onClick={send}
            >
              {sending ? "Queueing…" : "Send"}
            </Button>
            </div>
            <p className="mt-1 text-micro text-ink-faint">
              Your message waits for the turn in flight to finish — it is never spliced into one.
            </p>
          </div>
        </>
      ) : null}
    </section>
  );
}

/** §3c #12: queued (next) → delivered → consumed, so the person sees exactly when it landed. */
function MessageStatePill({
  message,
  messages,
  agentId,
  agentStatus,
}: {
  message: Message;
  messages: Message[];
  agentId: string;
  agentStatus?: AgentStatus;
}) {
  const d = displayState(message, messages, agentId, agentStatus);
  const color =
    d.tone === "error"
      ? "var(--danger)"
      : d.tone === "done"
        ? "var(--ink-faint)"
        : d.tone === "live"
          ? "var(--live)"
          : d.tone === "next"
            ? "var(--wire-send)"
            : "var(--ink-dim)";
  return (
    <span
      data-testid={`msg-${message.id}-state`}
      data-state={d.state}
      title={d.detail}
      className="rounded-control border px-1 leading-[1.4]"
      style={{ color, borderColor: color }}
    >
      {d.label}
    </span>
  );
}

const TRANSCRIPT_EMPTY =
  "Nothing written to this agent's stdin yet. Every message it receives shows up here, byte for byte.";
