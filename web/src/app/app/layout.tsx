// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { SessionGate } from "@/components/auth/session-gate";

/** Everything under /app needs a session. In clerk mode middleware says so; in local mode this does. */
export default function AppLayout({ children }: { children: React.ReactNode }) {
  return <SessionGate>{children}</SessionGate>;
}
