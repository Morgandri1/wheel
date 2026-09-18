// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { acceptInvite } from "@/lib/invite-routes";

/** `POST /api/invites/accept`. See `src/lib/invite-routes.ts`. */
export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export const POST = acceptInvite;
