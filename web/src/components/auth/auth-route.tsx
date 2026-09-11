"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import dynamic from "next/dynamic";
import { Suspense } from "react";
import { AuthScreen } from "@/components/auth/auth-screen";
import { authMode } from "@/lib/auth";

// Loaded only where it is actually rendered. The mode is decided by the server at run time, so the
// bundler cannot prove the other branch is dead — this says so.
const ClerkScreen = dynamic(() => import("@/components/auth/clerk-screen"), { ssr: false });

/** /sign-in and /sign-up serve whichever provider is configured, at the same two URLs. */
export function AuthRoute({ mode }: { mode: "sign-in" | "sign-up" }) {
  if (authMode() === "clerk") return <ClerkScreen mode={mode} />;
  return (
    <Suspense fallback={null}>
      <AuthScreen mode={mode} />
    </Suspense>
  );
}
