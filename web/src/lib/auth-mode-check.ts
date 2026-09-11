// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * Does the web server's auth mode agree with the API's?
 *
 * The web server reads WHEEL_AUTH_MODE and the API reads AUTH_MODE, set in different dashboards
 * or compose files by different people. When they disagree nobody can log in, and no test
 * either lane runs alone can see it — the client is correct, the server is correct, and the pair is
 * broken. `GET /healthz` reporting `auth_mode` (API, 52577ad) is what makes the pair checkable.
 *
 * The two vocabularies are NOT the same words, which is the trap: the client's `clerk` is the
 * server's `jwks`. A string equality check here would report a false mismatch on a correct
 * deployment, and get itself ignored.
 */
export type ClientAuthMode = "mock" | "dev" | "local" | "clerk";
export type ServerAuthMode = "local" | "jwks";

const EXPECTED: Record<ClientAuthMode, ServerAuthMode | null> = {
  local: "local",
  clerk: "jwks",
  // A pre-minted HS256 token is what the API's own `local` verifier accepts.
  dev: "local",
  // `mock` never talks to a real API. If one answered, we are pointed somewhere we should not be.
  mock: null,
};

export function authModeMismatch(
  client: ClientAuthMode,
  server: ServerAuthMode,
): string | null {
  if (client === "mock") {
    return `The web server is in mock auth mode but a real API answered, and it reports ${server}. Mock sends a fixed fake token that a real API will reject. WHEEL_AUTH_MODE is probably unset — it defaults to mock.`;
  }
  const expected = EXPECTED[client];
  if (expected === server) return null;
  return `The web server is running ${client} auth, which needs an API in ${expected} mode, but the API reports ${server}. Nobody will be able to log in. Fix whichever is wrong: WHEEL_AUTH_MODE on the web server, or AUTH_MODE on the API.`;
}

/** Reads the field the API actually sends; anything else is not an answer we can act on. */
export function serverAuthMode(health: unknown): ServerAuthMode | null {
  const mode = (health as { auth_mode?: unknown })?.auth_mode;
  return mode === "local" || mode === "jwks" ? mode : null;
}
