"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * A collapsed sidebar leaves a rail behind rather than disappearing.
 *
 * A panel that vanishes with no visible way back is a panel the user has lost, not closed — they
 * have to guess a keyboard shortcut or reload. The rail is the affordance that makes collapsing
 * safe to try.
 */
export function SidebarRail({
  side,
  label,
  onExpand,
  testId,
}: {
  side: "left" | "right";
  label: string;
  onExpand: () => void;
  testId: string;
}) {
  return (
    <button
      type="button"
      onClick={onExpand}
      data-testid={testId}
      title={`Show ${label}`}
      aria-label={`Show ${label}`}
      aria-expanded={false}
      className={`flex w-7 shrink-0 items-center justify-center bg-[var(--panel-1)] text-ink-faint transition-colors hover:text-ink ${
        side === "left" ? "border-r border-rule" : "border-l border-rule"
      }`}
    >
      <span className="[writing-mode:vertical-rl] text-micro tracking-wide" style={{ rotate: side === "left" ? "180deg" : "none" }}>
        {label}
      </span>
    </button>
  );
}

/** The control that collapses a panel, sitting inside the panel's own header. */
export function CollapseButton({
  side,
  label,
  onCollapse,
  testId,
}: {
  side: "left" | "right";
  label: string;
  onCollapse: () => void;
  testId: string;
}) {
  return (
    <button
      type="button"
      onClick={onCollapse}
      data-testid={testId}
      title={`Hide ${label}`}
      aria-label={`Hide ${label}`}
      aria-expanded
      className="shrink-0 px-1 text-ink-faint transition-colors hover:text-ink"
    >
      {side === "left" ? "‹" : "›"}
    </button>
  );
}
