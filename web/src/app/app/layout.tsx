import { SessionGate } from "@/components/auth/session-gate";

/** Everything under /app needs a session. In clerk mode middleware says so; in local mode this does. */
export default function AppLayout({ children }: { children: React.ReactNode }) {
  return <SessionGate>{children}</SessionGate>;
}
