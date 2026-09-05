"use client";

import { forwardRef, useEffect, useId, useRef, useState, type ReactNode } from "react";

export function cx(...parts: (string | false | null | undefined)[]) {
  return parts.filter(Boolean).join(" ");
}

// ---------------------------------------------------------------- button

type ButtonTone = "default" | "primary" | "danger" | "ghost";

const TONE: Record<ButtonTone, string> = {
  default:
    "bg-[var(--panel-2)] text-ink border-rule hover:border-[var(--rule-strong)] active:translate-y-px",
  primary:
    "bg-[var(--ink)] text-[var(--panel-1)] border-transparent hover:opacity-90 active:translate-y-px",
  danger:
    "bg-transparent text-[var(--danger)] border-[color-mix(in_srgb,var(--danger)_45%,transparent)] hover:bg-[color-mix(in_srgb,var(--danger)_10%,transparent)]",
  ghost: "bg-transparent text-ink-dim border-transparent hover:text-ink hover:bg-[var(--panel-2)]",
};

export const Button = forwardRef<
  HTMLButtonElement,
  React.ButtonHTMLAttributes<HTMLButtonElement> & { tone?: ButtonTone; size?: "sm" | "md" }
>(function Button({ tone = "default", size = "md", className, ...rest }, ref) {
  return (
    <button
      ref={ref}
      className={cx(
        "inline-flex items-center justify-center gap-1.5 rounded-control border font-medium",
        "transition-colors duration-100 ease-snap disabled:opacity-40 disabled:pointer-events-none",
        size === "sm" ? "h-7 px-2.5 text-micro" : "h-8 px-3 text-meta",
        TONE[tone],
        className,
      )}
      {...rest}
    />
  );
});

// ---------------------------------------------------------------- field

export function Field({
  label,
  hint,
  error,
  children,
  htmlFor,
}: {
  label: string;
  hint?: string;
  error?: string | null;
  children: ReactNode;
  htmlFor?: string;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={htmlFor} className="text-micro font-medium text-ink-dim">
        {label}
      </label>
      {children}
      {error ? (
        <p className="text-micro text-[var(--danger)]">{error}</p>
      ) : hint ? (
        <p className="text-micro text-ink-faint">{hint}</p>
      ) : null}
    </div>
  );
}

const CONTROL =
  "w-full rounded-control border border-rule bg-[var(--panel-0)] px-2.5 py-1.5 text-meta text-ink " +
  "placeholder:text-ink-faint focus:border-[var(--wire-read)] focus:outline-none";

export const Input = forwardRef<HTMLInputElement, React.InputHTMLAttributes<HTMLInputElement> & { mono?: boolean }>(
  function Input({ className, mono, ...rest }, ref) {
    return <input ref={ref} className={cx(CONTROL, mono && "ident", className)} {...rest} />;
  },
);

export const Textarea = forwardRef<
  HTMLTextAreaElement,
  React.TextareaHTMLAttributes<HTMLTextAreaElement> & { mono?: boolean }
>(function Textarea({ className, mono, ...rest }, ref) {
  return <textarea ref={ref} className={cx(CONTROL, "resize-y", mono && "ident", className)} {...rest} />;
});

export const Select = forwardRef<HTMLSelectElement, React.SelectHTMLAttributes<HTMLSelectElement>>(
  function Select({ className, ...rest }, ref) {
    return <select ref={ref} className={cx(CONTROL, "h-8 py-0", className)} {...rest} />;
  },
);

// ---------------------------------------------------------------- toggle

export function Toggle({
  checked,
  onChange,
  label,
  hint,
  testId,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  hint?: string;
  testId?: string;
}) {
  const id = useId();
  return (
    <div className="flex items-start gap-2.5">
      <button
        id={id}
        role="switch"
        aria-checked={checked}
        data-testid={testId}
        onClick={() => onChange(!checked)}
        className={cx(
          "mt-0.5 h-4 w-7 shrink-0 rounded-full border transition-colors duration-120 ease-snap",
          checked ? "border-transparent bg-[var(--live)]" : "border-rule bg-[var(--panel-2)]",
        )}
      >
        <span
          className={cx(
            "block h-3 w-3 rounded-full bg-[var(--panel-1)] transition-transform duration-120 ease-snap",
            checked ? "translate-x-3.5" : "translate-x-0.5",
          )}
        />
      </button>
      <div className="min-w-0">
        <label htmlFor={id} className="block cursor-pointer text-meta text-ink">
          {label}
        </label>
        {hint ? <p className="text-micro text-ink-faint">{hint}</p> : null}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------- pill

export function Pill({
  color,
  pulse,
  children,
  testId,
}: {
  color: string;
  pulse?: boolean;
  children: ReactNode;
  testId?: string;
}) {
  return (
    <span
      data-testid={testId}
      className="inline-flex items-center gap-1.5 text-micro text-ink-dim"
    >
      <span className="relative flex h-1.5 w-1.5">
        {pulse ? (
          <span
            className="absolute inline-flex h-full w-full animate-ping rounded-full opacity-60"
            style={{ background: color }}
          />
        ) : null}
        <span className="relative inline-flex h-1.5 w-1.5 rounded-full" style={{ background: color }} />
      </span>
      {children}
    </span>
  );
}

// ---------------------------------------------------------------- dialog

export function Dialog({
  open,
  onClose,
  title,
  children,
  testId,
}: {
  open: boolean;
  onClose: () => void;
  title: string;
  children: ReactNode;
  testId?: string;
}) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("keydown", onKey);
    ref.current?.querySelector<HTMLElement>("input,button,textarea")?.focus();
    return () => document.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-[rgba(0,0,0,0.45)] p-4"
      onMouseDown={(e) => e.target === e.currentTarget && onClose()}
    >
      <div ref={ref} data-testid={testId} role="dialog" aria-label={title} className="plate w-full max-w-md p-5">
        <h2 className="mb-3 text-lead font-semibold">{title}</h2>
        {children}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------- copy field

export function CopyField({ value, testId }: { value: string; testId?: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="flex items-stretch gap-1.5">
      <code
        data-testid={testId}
        className="ident min-w-0 flex-1 overflow-x-auto whitespace-nowrap rounded-control border border-rule bg-[var(--panel-0)] px-2.5 py-1.5 text-ink-dim"
      >
        {value}
      </code>
      <Button
        size="sm"
        onClick={async () => {
          await navigator.clipboard.writeText(value);
          setCopied(true);
          setTimeout(() => setCopied(false), 1400);
        }}
      >
        {copied ? "Copied" : "Copy"}
      </Button>
    </div>
  );
}

// ---------------------------------------------------------------- empty / loading

export function Empty({
  title,
  body,
  action,
}: {
  title: string;
  body: string;
  action?: ReactNode;
}) {
  return (
    <div className="flex flex-col items-start gap-2 border border-dashed border-rule p-6">
      <h3 className="text-lead font-semibold">{title}</h3>
      <p className="max-w-[52ch] text-meta text-ink-dim">{body}</p>
      {action ? <div className="mt-2">{action}</div> : null}
    </div>
  );
}

export function Skeleton({ className }: { className?: string }) {
  return <div className={cx("animate-pulse bg-[var(--panel-2)]", className)} />;
}

export function Glyph({ path, size = 16, color }: { path: string; size?: number; color?: string }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke={color ?? "currentColor"}
      strokeWidth={1.4}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      {path.split(" M").map((d, i) => (
        <path key={i} d={i === 0 ? d : `M${d}`} />
      ))}
    </svg>
  );
}
