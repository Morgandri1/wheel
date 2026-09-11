// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { sessionAction } from "@/lib/session-routes";

/** `POST /api/session/{login|signup|logout|password}`. See `src/lib/session-routes.ts`. */
export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(req: Request, { params }: { params: Promise<{ action: string }> }) {
  const { action } = await params;
  return sessionAction(req, action);
}
