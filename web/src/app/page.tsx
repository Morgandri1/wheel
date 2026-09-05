import Link from "next/link";
import { WheelMark } from "@/components/header";

/**
 * Placeholder. The real landing — board-as-hero with the message packet travelling
 * ctx → researcher → writer → endpoint — is M2, per docs/plans/web.md.
 */
export default function Landing() {
  return (
    <main className="mx-auto flex min-h-screen max-w-3xl flex-col justify-center px-6">
      <div className="flex items-center gap-2 text-ink-dim">
        <WheelMark size={22} />
        <span className="display text-lead font-semibold">wheel</span>
      </div>
      <h1 className="display mt-6 text-h1">
        They say don&apos;t reinvent the wheel.
        <br />
        Sometimes you have to.
      </h1>
      <p className="mt-5 max-w-[62ch] text-lead text-ink-dim">
        A board of agents, wired to exactly what they&apos;re allowed to touch, running in a
        container that keeps going after you close the tab.
      </p>
      <div className="mt-8">
        <Link
          href="/app"
          data-testid="cta-app"
          className="inline-flex h-9 items-center rounded-control bg-[var(--ink)] px-4 text-meta font-medium text-[var(--panel-1)]"
        >
          Open the board
        </Link>
      </div>
    </main>
  );
}
