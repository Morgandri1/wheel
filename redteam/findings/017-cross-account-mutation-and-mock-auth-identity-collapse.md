# 017 — Cross-account project mutation: NOT reproduced; but dev API collapses all identities to `user_mock`

- **Severity:** the cross-account-delete hypothesis is **NOT a finding** (source enforces ownership). The
  identity-collapse observation is **Medium (S2) — deployment risk**: a mock/dev-auth build in production
  would be a Critical universal-tenancy breach, so it must be structurally impossible to deploy.
- **Owner:** API.
- **Status:** INVESTIGATED / RESOLVED-NO-VULN for the delete question. Live on API `:8787`, 2026-09-05,
  two of my own dev accounts. PoC: `redteam/pocs/api-tenancy/t_cross_account_mutation.py` (now guarded).
- **Boundary:** TB1 (browser/API tenancy).

## The question (PM, S1)
Can user B delete/patch/start/stop user A's project? PM feared `vault-verify` (API's account) was deleted
cross-account.

## What happened, and the trap I nearly fell into
First pass: B's DELETE/PATCH/stop/start/restart on A's project ALL returned 2xx, A's project vanished,
A's name became `pwned`, and B could proxy into A's engine (`/engine/v1/board` → 200). Looked like a total
tenancy collapse — a Critical.

**Decisive check before reporting:** the `owner_id` the API stamps on each token's OWN project.
Both `alice` (sub=`adv_alice`) and `mallory` (sub=`adv_mallory`) tokens came back with
**`owner_id = "user_mock"`**. The `:8787` instance runs a MOCK auth that **ignores the JWT `sub`** and
assigns a constant identity. So my "two accounts" were ONE identity; every mutation succeeded because the
ownership predicate (`WHERE owner_id = $2`) genuinely matched — not because ownership was skipped. **No
cross-account access was demonstrated.** (Identical project lists for A and B were the tell, but ambiguous
between "no owner filter" and "same owner"; the `owner_id` comparison disambiguated.)

## Source is correct (verified by reading it, HEAD @ origin/main)
- `auth/claims.rs`: `VerifiedUser.user_id = claims.sub` (the real token subject).
- `auth/extractor.rs`: `ProjectScope` loads via `load_owned` = `WHERE id=$1 AND owner_id=$2` → RowNotFound
  → 404 on mismatch; comment: a handler that forgets ownership "cannot be written."
- `routes/projects.rs`: `get_one/patch/delete/start/stop/restart` all take `ProjectScope`; `list` filters
  `WHERE owner_id=$1`; `DELETE ... WHERE id=$1 AND owner_id=$2`. Ownership is a WHERE-predicate everywhere.
On a build that honors `sub`, cross-account is refused by construction.

## The real risk (Medium) + why it bears on vault-verify
The `:8787` build resolves EVERY token to `owner_id="user_mock"`. If a mock/dev-auth build like this ever
serves a shared/production instance, ALL accounts collapse to one identity and any authenticated caller can
read/mutate/**delete** any project and proxy into any engine — indistinguishable from the cross-account
deletion PM is investigating. So the question for `vault-verify` is not "is there an authz bug" (source: no)
but **"does the instance hosting vault-verify resolve distinct `sub`s, or is it identity-collapsed like
:8787?"** Test on that instance: sign up two accounts, POST a project as each, compare `owner_id`. Identical
⇒ identity collapse ⇒ S1 on that deployment, and it explains the deletion.

## Fixes / asks (API)
1. **Mock/constant-identity auth must be structurally impossible outside dev** (aligns with review R2): boot
   must refuse any build/config that would stamp a constant `owner_id`, and `WHEEL_ENV=prod` must never
   accept the dev HS256 path (config.rs already refuses `AUTH_DEV_SECRET` with prod — extend the same
   fail-closed stance to whatever produces `user_mock`).
2. Point me at a **sub-honoring** instance (real local signup/login, or prod with two of my own accounts) and
   I will run the true cross-account matrix (`t_cross_account_mutation.py`, now guarded to abort on identity
   collapse rather than mislabel it).

## Lesson (logged)
Nearly reported a false Critical S1. The `owner_id`-distinctness guard is now baked into the probe: it
aborts with INVALID-STACK if both tokens map to one owner, so a single-user delete can never again be
mislabeled cross-account. Verify principals are distinct BEFORE asserting "cross-account."
