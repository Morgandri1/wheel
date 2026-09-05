"use client";

/**
 * Hands Clerk's session token to the plain-function API client. Rendered only when
 * NEXT_PUBLIC_AUTH_MODE=clerk, so the app runs with no Clerk instance configured.
 */
import { useEffect } from "react";
import { useAuth } from "@clerk/nextjs";
import { setTokenGetter } from "@/lib/auth";

export function ClerkTokenBridge() {
  const { getToken } = useAuth();
  useEffect(() => {
    setTokenGetter(() => getToken());
  }, [getToken]);
  return null;
}
