"use client";

/** The board's key. Wires mean permissions, so the legend says what each one grants. */
export function Legend() {
  const rows = [
    { label: "read", color: "var(--wire-read)", note: "may look at it", width: 1.5, dash: "" },
    { label: "write", color: "var(--wire-write)", note: "may change it", width: 2.6, dash: "" },
    { label: "send", color: "var(--wire-send)", note: "may message it", width: 1.5, dash: "5 4" },
    { label: "inject", color: "var(--wire-send)", note: "prepended to the prompt", width: 1.4, dash: "1 5" },
  ];

  return (
    <div className="plate absolute bottom-3 left-3 z-10 px-2.5 py-2" data-testid="wire-legend">
      <ul className="flex flex-col gap-1">
        {rows.map((r) => (
          <li key={r.label} className="flex items-center gap-2 text-micro text-ink-dim">
            <svg width="26" height="8" aria-hidden>
              <line
                x1="1"
                y1="4"
                x2="25"
                y2="4"
                stroke={r.color}
                strokeWidth={r.width}
                strokeDasharray={r.dash || undefined}
                strokeLinecap="round"
              />
            </svg>
            <span style={{ color: r.color }}>{r.label}</span>
            <span className="text-ink-faint">{r.note}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}
