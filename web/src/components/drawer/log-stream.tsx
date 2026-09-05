"use client";

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { LogLine } from "@/lib/schema";

const ROW = 18;
const OVERSCAN = 12;

const STREAM_COLOR: Record<string, string> = {
  stdout: "var(--ink)",
  stderr: "var(--danger)",
  engine: "var(--ink-faint)",
  // The transcript is what the agent was handed, not what it said — a distinct voice.
  transcript: "var(--wire-send)",
};

/**
 * Windowed log. Rows are a fixed height and never wrap (they scroll sideways instead), so a
 * running agent can emit thousands of lines without the tab paying for them.
 */
export function LogStream({ lines, empty }: { lines: LogLine[]; empty?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [height, setHeight] = useState(240);
  const [pinned, setPinned] = useState(true);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    setHeight(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    const el = ref.current;
    if (el && pinned) el.scrollTop = el.scrollHeight;
  }, [lines.length, pinned]);

  const start = Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN);
  const end = Math.min(lines.length, Math.ceil((scrollTop + height) / ROW) + OVERSCAN);
  const visible = lines.slice(start, end);

  if (!lines.length) {
    return (
      <div className="flex h-full items-center px-3 text-micro text-ink-faint" data-testid="log-empty">
        {empty ?? "Nothing yet. Start the agent, or send it a message."}
      </div>
    );
  }

  return (
    <div className="relative h-full">
      <div
        ref={ref}
        data-testid="log-stream"
        className="h-full overflow-auto bg-[var(--panel-0)]"
        onScroll={(e) => {
          const el = e.currentTarget;
          setScrollTop(el.scrollTop);
          setPinned(el.scrollHeight - el.scrollTop - el.clientHeight < 24);
        }}
      >
        <div style={{ height: lines.length * ROW, position: "relative" }}>
          <div style={{ transform: `translateY(${start * ROW}px)` }}>
            {visible.map((l) => (
              <div
                key={l.seq}
                data-testid="log-line"
                data-stream={l.stream}
                className="ident whitespace-pre px-3"
                style={{ height: ROW, lineHeight: `${ROW}px`, color: STREAM_COLOR[l.stream] ?? "var(--ink)" }}
              >
                {l.text || " "}
              </div>
            ))}
          </div>
        </div>
      </div>

      {!pinned ? (
        <button
          data-testid="btn-log-follow"
          onClick={() => {
            const el = ref.current;
            if (el) el.scrollTop = el.scrollHeight;
            setPinned(true);
          }}
          className="plate absolute bottom-2 right-3 px-2 py-1 text-micro text-ink-dim hover:text-ink"
        >
          Jump to latest
        </button>
      ) : null}
    </div>
  );
}
