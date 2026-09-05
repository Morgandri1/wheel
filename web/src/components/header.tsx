"use client";

import Link from "next/link";
import { AUTH_MODE } from "@/lib/auth";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui";

/** The mark: a wheel drawn as a hub with spokes terminating in connection points. */
export function WheelMark({ size = 18 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" aria-hidden>
      <circle cx="12" cy="12" r="9.25" stroke="currentColor" strokeWidth="1.4" />
      <circle cx="12" cy="12" r="2.4" stroke="currentColor" strokeWidth="1.4" />
      {[0, 60, 120, 180, 240, 300].map((a) => (
        <line
          key={a}
          x1={12 + 2.4 * Math.cos((a * Math.PI) / 180)}
          y1={12 + 2.4 * Math.sin((a * Math.PI) / 180)}
          x2={12 + 9.25 * Math.cos((a * Math.PI) / 180)}
          y2={12 + 9.25 * Math.sin((a * Math.PI) / 180)}
          stroke="currentColor"
          strokeWidth="1.1"
        />
      ))}
    </svg>
  );
}

export function ThemeSwitch() {
  const [theme, setTheme] = useState<"dark" | "light">("dark");
  useEffect(() => {
    const stored = localStorage.getItem("wheel-theme");
    const next = stored === "light" ? "light" : "dark";
    setTheme(next);
    document.documentElement.dataset.theme = next;
  }, []);
  return (
    <Button
      tone="ghost"
      size="sm"
      data-testid="btn-theme"
      aria-label={theme === "dark" ? "Switch to light" : "Switch to dark"}
      onClick={() => {
        const next = theme === "dark" ? "light" : "dark";
        setTheme(next);
        document.documentElement.dataset.theme = next;
        localStorage.setItem("wheel-theme", next);
      }}
    >
      {theme === "dark" ? "Light" : "Dark"}
    </Button>
  );
}

export function Header({ children }: { children?: React.ReactNode }) {
  return (
    <header className="flex h-12 shrink-0 items-center gap-3 border-b border-rule bg-[var(--panel-1)] px-4">
      <Link href="/app" className="flex items-center gap-2 text-ink" data-testid="link-home">
        <WheelMark />
        <span className="display text-meta font-semibold tracking-tight">wheel</span>
      </Link>
      <div className="flex-1">{children}</div>
      <ThemeSwitch />
      {AUTH_MODE === "mock" ? (
        <span className="text-micro text-ink-faint" data-testid="auth-mode">
          mock auth
        </span>
      ) : null}
    </header>
  );
}
