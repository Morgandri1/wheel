"use client";

import dynamic from "next/dynamic";
import { Suspense } from "react";
import { AuthScreen } from "@/components/auth/auth-screen";
import { AUTH_MODE } from "@/lib/auth";

// Loaded only where it is actually rendered. `AUTH_MODE` is read from the environment rather than
// written as a literal, so the bundler cannot prove the other branch is dead — this says so.
const ClerkScreen = dynamic(() => import("@/components/auth/clerk-screen"), { ssr: false });

/** /sign-in and /sign-up serve whichever provider is configured, at the same two URLs. */
export function AuthRoute({ mode }: { mode: "sign-in" | "sign-up" }) {
  if (AUTH_MODE === "clerk") return <ClerkScreen mode={mode} />;
  return (
    <Suspense fallback={null}>
      <AuthScreen mode={mode} />
    </Suspense>
  );
}
