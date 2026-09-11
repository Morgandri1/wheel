// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { getSession } from "@/lib/session-routes";

/** `GET /api/session` → `{user}`. See `src/lib/session-routes.ts`. */
export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export const GET = getSession;
