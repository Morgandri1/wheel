# Review — infra/prune-probe-projects.sh (a production DELETION tool) — ADVERSARY: APPROVED

- **Verdict:** APPROVED for `--apply`. All six PM criteria hold; API's 36-assertion suite passes and my
  independent adversarial checks of every safety predicate pass. One residual (Low, policy) noted.
- **Reviewed:** `infra/prune-probe-projects.sh` (157 lines) + `infra/tests/prune-probe-projects.test.sh`
  (36 assertions, incl. a full DB+host-stubbed run). Owner: API. Boundary TB1 (tenant data — this tool can
  delete projects, the vault-verify-deletion class of risk).

## The six criteria — all met
1. **Deny list checked before every other predicate.** `is_candidate` runs `is_denied` FIRST and returns
   not-a-candidate on a match, before uuid/probe/age. Confirmed: a denied project id AND a denied owner are
   both non-candidates even when probe-domain + old.
2. **Empty deny list refuses.** `require_deny_list` (called in `main` before the fetch) refuses if EITHER
   `DENY_OWNERS` or `DENY_PROJECTS` is empty — whitespace-stripped (`${VAR// /}`), so a spaces-only list also
   refuses. Confirmed both.
3. **Exact domain match, never suffix.** `is_probe_address` takes the last-`@` segment and compares by exact
   string equality against `PROBE_DOMAINS`. Confirmed: `a@wheel.test` → probe; `a@wheel.test.evil.org` (suffix)
   → NO; `a@notwheel.test` (prefix) → NO; empty email → NO. The last-`@` rule can only make an address MATCH a
   probe domain (never protect a real project), and deny-list is checked first regardless.
4. **uuid-only into SQL.** The only value interpolated into SQL is the project id, and `is_candidate` requires
   `is_uuid` (8-4-4-4-12 hex) before a row can be a candidate → `delete_row`/`destroy_sandbox` only ever see a
   validated uuid. The owner email is NEVER in any SQL (only compared for deny/probe + printed). Confirmed: a
   `'; DROP TABLE projects;--` id and a non-uuid id are both non-candidates.
5. **Sandbox destroyed before the row.** `destroy_sandbox` (host DELETE) runs first; `delete_row` runs ONLY on
   a 2xx; any non-2xx keeps the row ("host said X — row kept"). No orphaned sandbox; safe to re-run (host
   destroy is idempotent).
6. **Dry-run default.** No arg / `--dry-run` = list only; `--apply` required to delete; any other arg → usage
   error (exit 2).

## Additional adversarial angles — all safe
- Negative age (clock skew → negative EPOCH) and non-numeric age → not old-enough → safe. Young (<24h) → safe.
- A project whose `owner_id` doesn't join to a users row → email `''` → not a probe → never a candidate
  (LEFT JOIN keeps it visible in the listing but unpruned). Correct.
- `set -euo pipefail`; `destroy_sandbox || echo 000` prevents a curl failure from aborting mid-loop while
  keeping the row.
- A legitimate probe (uuid + probe domain + old + not denied) IS a candidate — so it is not accidentally
  "deny everything," which would make the tool useless and hide a real regression.

## Residual (Low, policy — not a tool bug)
`example.com` is in `PROBE_DOMAINS`. It is a real, widely-used domain (RFC 2606 reserved, but people do sign
up demo/personal accounts with it). Any real project owned by an `@example.com` address WOULD be pruned once
older than a day, unless its id/owner is on the deny list. This is API's intended policy (probe accounts
should use these domains) and is mitigated by the deny list, but worth stating: prefer a probe-only domain
(`wheel.test`/`wheelcheck.dev`) for anything that must survive, and keep `example.com` only if no real account
will ever use it. No change required for approval.

## Bottom line
Deletion-safe by construction: deny-first + empty-deny-refuses + exact-domain + uuid-only-SQL +
destroy-before-row + dry-run-default all verified live. Approved to `--apply`.
