# 055 — A single recipient's concurrent double-accept spends two uses of a shared invite

- **Severity:** Low (narrow, self-limiting, not a privilege or revocation bypass)
- **Type:** Residual found while verifying F4/F5's fix (PR #70) — not F4 or F5 themselves, a
  different gap in the same function.
- **Owner:** API (`membership.rs::accept`)
- **Status:** OPEN

## Claim

`accept()`'s "never lower an existing tier" check (`find(db, &project_id, user_id)`, the `if let
Some(existing) = ...` block just before the consuming `UPDATE`) is a plain `SELECT`, not part of the
atomic consumption step. Two concurrent `accept()` calls for the **same token and same user** (a
double-click, a retried request, or a deliberate script) can both observe "not yet a member" from
that `SELECT` before either has called `grant()`, so both proceed to the atomic `uses < max_uses`
`UPDATE` — which correctly lets both succeed, since each is a genuinely separate row-update against
a counter that had two spare uses. `grant()`'s `INSERT ... ON CONFLICT DO UPDATE` is idempotent, so
the resulting membership row is correct either way (no privilege issue) — but the invite's `uses`
counter is charged twice for one person joining once.

Verified live (not just read): a throwaway test racing two `accept()` calls for the same token and
the same newly-signed-up user against a `max_uses: 5` invite returned `200` for both, and the
invite's own `uses` field read back as `2` afterward.

## Why this is a different finding from F4/F5

F4 was the email-lock check running *after* the use was already spent, burning a stranger's failed
attempt against the real recipient's budget. F5 was a revoked member's old link still working at
all. Both are now correctly ordered relative to the one atomic consuming statement (see PR #70's
`a_wrong_email_attempt_on_a_locked_invite_does_not_spend_a_use` /
`a_revoked_member_cannot_walk_back_in_with_the_invite_they_joined_on`, both mutation-checked). This
finding is about a *third* non-mutating check — "does this exact user already hold this exact
membership" — racing against the SAME atomic step from the SAME caller, which neither F4 nor F5's
fix addressed because neither was about self-racing.

## Impact

A `max_uses` invite is meant to be shared among several intended recipients (the API's own docs
describe it as the mechanism for that). One recipient — accidentally via a double-click/retry, or
deliberately — can consume more than one slot of a link meant for several people, denying access to
whoever was supposed to get the last slot. Not a security bypass (nobody gains a tier or membership
they should not have, and nobody's existing access is affected), but a real griefing vector against
*other* invitees sharing one link.

## Recommendation

Not fixed here — out of the F4/F5 scope this was found while verifying, and closing it properly
needs either a real transaction wrapping the existing-tier read and the consuming `UPDATE` (nothing
else in this file uses an explicit transaction; `Db`'s wrapper would need to expose one), or folding
the "does this user already have a live row" check into the same atomic statement as the consumption
(harder, since it spans two tables). Cheapest interim mitigation worth considering: make the
consuming `UPDATE` conditional on `NOT EXISTS (SELECT 1 FROM project_members WHERE project_id = ...
AND user_id = ... AND revoked_at IS NULL)`, folding the two tables' checks into one statement rather
than two round trips — API's call on whether that is worth doing ahead of a real transaction API.

## What would change my mind

If invites are only ever meant to be genuinely single-use in practice regardless of the configurable
`max_uses` field (i.e., nobody actually relies on sharing one link with several people), this
narrows to "a client can retry-spend its own budget," which is closer to a UX footgun than a
security-relevant finding. Worth confirming the intended usage pattern before prioritizing a fix.
