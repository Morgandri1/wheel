"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { setAuthMode, type AuthMode } from "@/lib/auth";

/**
 * Carries the server's run-time auth mode into the client bundle. It is the ONLY piece of server
 * configuration the browser receives; the API's address stays on the server.
 *
 * It records during render rather than in an effect on purpose: effects run after children have
 * rendered, and a child that branches on the mode would already have read the default. Setting a
 * module variable is idempotent, so a re-render costs nothing.
 */
export function RuntimeConfig({ authMode }: { authMode: AuthMode }) {
  setAuthMode(authMode);
  return null;
}
