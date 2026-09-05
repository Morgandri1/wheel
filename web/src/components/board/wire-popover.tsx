"use client";

import { useEffect, useRef } from "react";
import { allowedWireTypes, impliesRead, wireRule } from "@/lib/wire-matrix";
import { WIRE_META } from "@/lib/node-meta";
import type { PendingWire } from "@/store/board";
import type { WireType } from "@/lib/schema";

/**
 * Offers only the wire types §3 permits between these two node types. An illegal pair never
 * reaches this popover — the connection is refused at the drag, with the reason.
 */
export function WirePopover({
  pending,
  error,
  onPick,
  onCancel,
}: {
  pending: PendingWire;
  /** An engine refusal for this wire. Shown here rather than in a toast — see below. */
  error?: { code: string; message: string } | null;
  onPick: (type: WireType) => void;
  onCancel: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const options = allowedWireTypes(pending.fromType, pending.toType);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onCancel();
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onCancel();
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onDown);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("mousedown", onDown);
    };
  }, [onCancel]);

  return (
    <div
      ref={ref}
      data-testid="wire-popover"
      className="plate fixed z-50 w-[290px] p-1"
      style={{ left: Math.min(pending.at.x, window.innerWidth - 310), top: pending.at.y }}
    >
      <p className="px-2 py-1.5 text-micro text-ink-faint">
        What may <span className="ident text-ink-dim">{pending.fromType}</span> do to{" "}
        <span className="ident text-ink-dim">{pending.toType}</span>?
      </p>
      {error ? <WireRefusal error={error} /> : null}

      <ul className="flex flex-col">
        {options.map((t) => {
          const rule = wireRule(pending.fromType, pending.toType, t)!;
          const meta = WIRE_META[t];
          return (
            <li key={t}>
              <button
                data-testid={`wire-option-${t}`}
                onClick={() => onPick(t)}
                className="flex w-full flex-col items-start gap-0.5 px-2 py-1.5 text-left transition-colors hover:bg-[var(--panel-2)]"
              >
                <span className="flex items-center gap-2">
                  <svg width="20" height="8" aria-hidden>
                    <line
                      x1="1"
                      y1="4"
                      x2="19"
                      y2="4"
                      stroke={meta.color}
                      strokeWidth={t === "write" ? 2.6 : 1.5}
                      strokeDasharray={meta.dash === "0" ? undefined : meta.dash}
                      strokeLinecap="round"
                    />
                  </svg>
                  <span className="text-meta" style={{ color: meta.color }}>
                    {meta.label}
                  </span>
                  {t === "write" && impliesRead(pending.fromType, pending.toType) ? (
                    <span className="text-micro text-ink-faint">includes read</span>
                  ) : null}
                </span>
                <span className="text-micro text-ink-dim">{rule.outgoing}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/**
 * Why the engine said no, in the place the person is still looking.
 *
 * A toast is wrong for this: it disappears while they are reading it, and every one of these
 * refusals needs an action from them — pick a different vault, rename a key, choose another wire
 * type. The engine's own message is shown verbatim because it is the authority and its wording
 * is specific; we only add the sentence that says what to DO, which the engine has no way to know.
 */
function WireRefusal({ error }: { error: { code: string; message: string } }) {
  // Contract: one vault per account, so an agent wired to two vaults holding the same key has no
  // defined answer for which token it gets. The engine refuses rather than picking one, and this
  // is the only refusal where the fix is not "choose a different wire".
  const ambiguous = /ambiguous credential/i.test(error.message) || error.code === "ambiguous_credential";
  return (
    <div
      data-testid="wire-error"
      data-code={error.code}
      className="mx-1 mb-1 border-l-2 px-2 py-1.5"
      style={{ borderColor: "var(--danger)" }}
    >
      <p className="text-micro" style={{ color: "var(--danger)" }}>
        {error.message}
      </p>
      {ambiguous ? (
        <p className="mt-1 text-micro text-ink-dim">
          Two vaults on this agent would supply the same key, and there is no correct answer for
          which one wins. Unwire the other vault, or rename the key in one of them — one vault per
          account is the pattern.
        </p>
      ) : null}
    </div>
  );
}
