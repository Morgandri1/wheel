# 017 — Running dev API on :8787 is a STALE MOCK build with ZERO tenant isolation (all owner_id='user_mock')

- **Severity:** High **as a dev-stack hazard / investigation lead**; **NOT a product vuln in current source.**
- **Owner:** API (dev/deploy hygiene). Current source is CORRECT — see below.
- **Status:** CONFIRMED (2026-09-05). The API answering on `127.0.0.1:8787` stamps `owner_id='user_mock'`
  on every project regardless of the authenticated caller, so any account can delete/patch/start/stop any
  project. `user_mock` does **not exist anywhere in `crates/wheel-api/src` at HEAD** → the binary is stale
  (pre-local-auth). PoCs: `redteam/pocs/api-tenancy/t_cross_account_local_auth.py` (+ the guarded
  `t_cross_account_mutation.py`).
- **Boundary:** TB1 (browser/API tenancy).

## What I observed (live, :8787)
Two genuinely distinct principals via real `POST /v1/auth/signup` (ids `da9eaac4…` and `4d94ee1a…`,
opaque `local.<uuid>` tokens). A created a project → **`owner_id` came back as `user_mock`** (not A's id).
Then B: `GET A → 200`, `DELETE A → 204`, and A's project was **gone**. On its face that is cross-account
delete. It is NOT: the instance resolves *every* token to the same mock identity `user_mock`, so A and B
are one principal there. (The guarded HS256 probe caught the same collapse first: both dev tokens →
`owner_id='user_mock'`.)

## Why it is NOT a product S1 (current source is fail-closed)
- `user_mock` appears in **zero** files under `crates/wheel-api/src` at HEAD.
- `auth/extractor.rs`: `AuthUser` derives the id from the **validated session** per `AUTH_MODE`
  (local → `verify_session` against `session_secret`+`public_base_url`; jwks → RS256). No mock path.
- Every handler acting on an existing project takes **`ProjectScope`** (`routes/projects.rs`:
  `get_one`, `update`, `destroy`, `start`, `stop`, `restart`). `create`/`list` take `AuthUser`. **No
  handler takes a bare project id.**
- `ProjectScope` → `load_owned(id, caller)` = the ONLY id→Project function:
  `SELECT … FROM projects WHERE id=$1 AND owner_id=$2`. A non-owner (or missing) row → `NotFound` (404).
  "Not yours" and "doesn't exist" are the same path → no enumeration oracle. This is textbook-correct.
- Therefore, on a HEAD build, B's `GET/DELETE/PATCH/start/stop/restart` of A's project all return 404.

## Relevance to PM's `vault-verify` question
If any project (e.g. `vault-verify`) lived on this :8787 mock instance, then **any account using :8787
could delete it** — because all projects there are owned by `user_mock`. That fully explains a
cross-account-looking deletion **without any bug in the shipping code**. The deletion is a stale-binary
artifact of :8787, not a flaw in HEAD.

## Recommendations
1. Rebuild/redeploy whatever serves :8787 from `>= 2cda5cc` (local-auth). The current binary predates it
   and has no tenant isolation; nobody should draw tenancy conclusions (or store real projects) on it.
2. **Confirm the PRODUCTION binary (Railway `wheel-api`) is built from HEAD, not this mock build.** If prod
   ever served `owner_id='user_mock'`, that is a true S1 — quick check: create a project on prod as your own
   account and confirm `owner_id` equals your real user id, not a constant.
3. Regression test (API): assert a created project's `owner_id` equals the authenticated caller's id, and
   that a second account gets 404 on GET/DELETE/PATCH/start/stop — so an identity-collapse build can never
   pass CI. (Complements the existing extractor design.)

## To fully close
Re-run `t_cross_account_local_auth.py` against a freshly built HEAD API → expect ALL RESISTED. I can build
+ run it on request; the code path above makes the outcome near-certain, but a live green closes it.
