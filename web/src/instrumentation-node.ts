// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { checkServerConfig } from "@/lib/runtime-config";

/** EX_CONFIG: the configuration, not the code, is what has to change. */
export const CONFIG_EXIT_CODE = 78;

/**
 * Logs what the server will use, or refuses to start. A server whose configuration would refuse
 * every request — a malformed WHEEL_API_URL, an unknown auth mode, a shared-credential mode in
 * production — must not come up looking healthy. Next on its own only logs a failed hook and goes
 * on serving 500s, so the refusal is made here.
 */
export function enforceServerConfig(): void {
  try {
    console.log(`wheel-web: ${checkServerConfig()}`);
  } catch (error) {
    console.error(`wheel-web: refusing to start. ${(error as Error).message}`);
    process.exit(CONFIG_EXIT_CODE);
  }
}
