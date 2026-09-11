// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * Configuration is validated when the server starts (`src/instrumentation-node.ts`). The check is
 * imported only on the Node runtime, the one that serves the API routes, so nothing it uses is
 * compiled into the Edge bundle.
 */
export async function register() {
  if (process.env.NEXT_RUNTIME === "nodejs") {
    const { enforceServerConfig } = await import("./instrumentation-node");
    enforceServerConfig();
  }
}
