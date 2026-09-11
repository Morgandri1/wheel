// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { proxyToApi } from "@/lib/api-proxy";

/** The browser's only road to the API. Rules and order of checks: `src/lib/api-proxy.ts`. */
export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export const GET = proxyToApi;
export const POST = proxyToApi;
export const PUT = proxyToApi;
export const PATCH = proxyToApi;
export const DELETE = proxyToApi;
