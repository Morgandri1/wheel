// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { relayEvents } from "@/lib/event-relay";

/** The engine event socket as Server-Sent Events. See `src/lib/event-relay.ts`. */
export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET(req: Request, { params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  return relayEvents(req, id);
}
