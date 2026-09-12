// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { relayBuilderTurn } from "@/lib/builder-relay";

/** One Workflow Builder turn, streamed. See `src/lib/builder-relay.ts`. */
export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(req: Request, { params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  return relayBuilderTurn(req, id);
}
