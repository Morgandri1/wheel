// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { probeIngress } from "@/lib/ingress-probe";

/** The endpoint panel's test button. See `src/lib/ingress-probe.ts`. */
export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export const POST = probeIngress;
