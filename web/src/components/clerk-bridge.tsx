"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * Clerk, mounted only when it is configured.
 *
 * ClerkProvider throws without a publishable key, so mock, dev and local modes must not render it.
 * That is why this is one component rather than a provider in the root layout: the whole Clerk
 * tree is conditional in a single place.
 *
 * There is no token bridge any more. Clerk keeps its session cookie fresh from here, and this
 * app's server reads the token with `auth().getToken()` when it calls the API; the browser never
 * handles it.
 */
import { ClerkProvider } from "@clerk/nextjs";

export function ClerkGate({ children }: { children: React.ReactNode }) {
  return <ClerkProvider>{children}</ClerkProvider>;
}
