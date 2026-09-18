/**
 * What a member may do in a shared project (operator ruling, 2026-09-11).
 *
 * Three tiers, ordered least to most capable. The whole comparison is index order, so there is no
 * table of pairwise rules to get wrong — the same shape as the Rust `Tier` enum, whose `Ord` is
 * derived from declaration order for the same reason.
 *
 * **This decides nothing.** The API enforces every tier independently, per route, default-deny; a
 * UI that hides a control is a courtesy to the user, never a boundary. What this is for is
 * rendering the right affordances, so a guest is not offered a button that will 403.
 */
export const TIERS = ["guest", "prompter", "admin"] as const;

export type Tier = (typeof TIERS)[number];

/**
 * Does `actual` reach `needed`?
 *
 * Anything this build does not recognise — `undefined`, `""`, a tier from a future version — is not
 * a tier and satisfies nothing. Rounding an unknown value *up* is how a later `viewer` would
 * silently become an admin, so the unknown case has to fall on the refusing side.
 *
 * There is deliberately no explicit `if (unknown) return false` guard, because there is nothing for
 * one to do: `indexOf` yields `-1` for an unknown value, `needed` is always a real tier so its index
 * is at least `0`, and `-1 >= 0` is already false. An added guard would be unreachable — and an
 * unreachable guard is worse than none, because it reads as the thing keeping you safe while the
 * test that "covers" it cannot fail. (It was written that way first, and the mutation check caught
 * it: removing the guard left every test green.)
 */
export function tierAtLeast(actual: string | undefined, needed: Tier): boolean {
  return TIERS.indexOf(actual as Tier) >= TIERS.indexOf(needed);
}

/** Human label for a tier, for anywhere the raw wire value would be unkind to read. */
export function tierLabel(tier: Tier): string {
  switch (tier) {
    case "admin":
      return "Admin";
    case "prompter":
      return "Prompter";
    case "guest":
      return "Guest";
  }
}
