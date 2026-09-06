"use client";

import { setApiBaseUrl } from "@/lib/runtime-config";

/**
 * Carries the server's resolved API URL into the client bundle.
 *
 * It records during render rather than in an effect on purpose: effects run after children have
 * rendered, and a child that fetches on mount would already have read the stale build-time value.
 * Setting a module variable is idempotent, so a re-render costs nothing.
 */
export function RuntimeConfig({ apiUrl }: { apiUrl: string }) {
  setApiBaseUrl(apiUrl);
  return null;
}
