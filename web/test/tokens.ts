// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/** Shaped like the API's session JWT. The web server never checks a signature, only shape and `exp`. */
export function jwtWith(claims: Record<string, unknown>): string {
  return `eyJhbGciOiJIUzI1NiJ9.${Buffer.from(JSON.stringify(claims)).toString("base64url")}.c2lnbmF0dXJl`;
}

export function liveJwt(claims: Record<string, unknown> = {}): string {
  return jwtWith({ exp: Math.floor(Date.now() / 1000) + 3600, ...claims });
}

export function expiredJwt(): string {
  return jwtWith({ exp: Math.floor(Date.now() / 1000) - 60 });
}
