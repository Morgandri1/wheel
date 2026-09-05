"use client";

/**
 * Clerk, mounted only when it is configured.
 *
 * ClerkProvider throws without a publishable key, and useAuth() throws without ClerkProvider —
 * so mock and dev modes must not render either. That is why this is one component rather than a
 * provider in the root layout: the whole Clerk tree is conditional in a single place.
 */
import { useEffect } from "react";
import { ClerkProvider, useAuth } from "@clerk/nextjs";
import { setTokenGetter } from "@/lib/auth";

/** Hands Clerk's session token to the plain-function API client, refetched per request. */
function TokenBridge() {
  const { getToken } = useAuth();
  useEffect(() => {
    setTokenGetter(() => getToken());
  }, [getToken]);
  return null;
}

export function ClerkGate({ children }: { children: React.ReactNode }) {
  return (
    <ClerkProvider>
      <TokenBridge />
      {children}
    </ClerkProvider>
  );
}
