"use client";

/** Errors say what happened and, where we can, what to do about it. No apologies, no mood. */
import { useSyncExternalStore } from "react";

export interface Toast {
  id: number;
  tone: "error" | "info";
  message: string;
}

let toasts: Toast[] = [];
const subs = new Set<() => void>();
let seq = 0;

function emit() {
  toasts = [...toasts];
  for (const s of subs) s();
}

export function toast(message: string, tone: Toast["tone"] = "info") {
  const id = ++seq;
  toasts.push({ id, tone, message });
  emit();
  setTimeout(() => {
    toasts = toasts.filter((t) => t.id !== id);
    emit();
  }, tone === "error" ? 6000 : 3200);
}

export function toastError(e: unknown, fallback = "That didn't work.") {
  toast((e as { message?: string })?.message || fallback, "error");
}

export function ToastHost() {
  const list = useSyncExternalStore(
    (cb) => {
      subs.add(cb);
      return () => subs.delete(cb);
    },
    () => toasts,
    () => toasts,
  );

  if (!list.length) return null;
  return (
    <div className="pointer-events-none fixed bottom-4 left-1/2 z-[60] flex -translate-x-1/2 flex-col gap-2">
      {list.map((t) => (
        <div
          key={t.id}
          data-testid="toast"
          data-tone={t.tone}
          className="plate pointer-events-auto max-w-[60ch] px-3 py-2 text-meta"
          style={
            t.tone === "error"
              ? { borderColor: "color-mix(in srgb, var(--danger) 50%, transparent)" }
              : undefined
          }
        >
          {t.message}
        </div>
      ))}
    </div>
  );
}
