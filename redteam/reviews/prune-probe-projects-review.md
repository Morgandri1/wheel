# Review — infra/prune-probe-projects.sh (destructive prod cleanup) — APPROVED

Red-team approval gate for a tool that DELETES projects on the deployed API (destroys the sandbox, then drops
the `projects` row). Reviewed @ origin/main d206b95 against the 6 criteria; the tool + its test
(`infra/tests/prune-probe-projects.test.sh`) meet all six. **Approved to run `--apply`.**

| # | Criterion | Verdict | Where |
|---|-----------|---------|-------|
| 1 | Deny list checked BEFORE every other predicate | ✅ | `is_candidate` calls `is_denied` first (`:71`), separately; test "deny list" 26-32 |
| 2 | Empty deny list refuses (fail-closed) | ✅ | `require_deny_list` (`:83`) refuses if EITHER `DENY_OWNERS`/`DENY_PROJECTS` is space-empty; called before the loop (`:118`); test 64-66 |
| 3 | Exact domain match, never suffix | ✅ | `is_probe_address` exact `[ "$domain" = "$d" ]` on `${email##*@}` (`:49`); tests REJECT `sub.wheel.test`, `wheel.test.attacker.com`, `notwheel.test`, ``, bare-domain |
| 4 | uuid-only into SQL | ✅ | `is_uuid` airtight 8-4-4-4-12 hex regex gates `delete_row`; `delete_row` only reached after `is_candidate`→`is_uuid`; test rejects `'; DELETE FROM projects; --` |
| 5 | Sandbox destroyed before the row | ✅ | `main` destroys first (`:140`), deletes the row ONLY on a 2xx (`:142`); a failed destroy KEEPS the row ("host said X — row kept") — fail-safe, no orphan |
| 6 | Dry-run default | ✅ | `apply=0` default; only `--apply` (`:113`); dry-run prints "(dry run)" and `continue`s (no destroy/delete); `--apply` also requires `WHEEL_HOST_URL`/`WHEEL_HOST_SECRET`; test proves dry-run destroys nothing |

## Extra safety worth crediting (beyond the 6)
- **Orphan-owner never deleted**: a project whose `owner_id` has no `users` row → `coalesce(email,'')` → `''`
  → `is_probe_address('')` false → never a candidate. Unknown owner = kept. (`:90-91`, test `:62`.)
- **MIN_AGE 86400s (1 day)** — nothing young is a candidate (test 60-61), so a probe just created by another
  run/session isn't nuked mid-flight.
- **Dual deny**: the operator's account by EMAIL (`morgan@avo.so`) AND the wheel-dev board by PROJECT ID
  (`6906cadb-…`), both case-insensitive — the two irreplaceable things are protected two independent ways.
- **Probe-domain-match is the PRIMARY gate**: only test-domain accounts are ever candidates, so any real user
  (non-probe domain) is safe regardless of the deny list; the deny list is the belt for the probe-adjacent two.
- **No cross-user deletion path**: to be deleted a project needs an owner email whose exact domain is a probe
  domain — an attacker cannot make ANOTHER user's email be at a probe domain, so this cannot be turned into a
  deletion of someone else's project. Registering one's OWN account at a probe domain only self-prunes.

## Non-blocking notes (do not gate approval)
1. **`example.com` in `PROBE_DOMAINS` is broad.** It's RFC-2606 reserved (safe in principle), but if the
   deployed signup ever ACCEPTS `@example.com` registrations, a real-ish account's project would be pruned
   after 1 day. Recommend: confirm signup rejects reserved/probe domains, or that `example.com` is intended.
2. **`delete_row` string-interpolates the uuid** into `DELETE … WHERE id = '$1'`. Safe because `is_uuid` admits
   only `[0-9a-f]` + dashes (no quote/semicolon can survive — the test proves the SQLi payload is rejected); a
   parameterized `psql -v` would be belt-and-suspenders, optional.
3. **`set -euo pipefail` + a mid-loop `delete_row` failure** would abort the run, leaving a destroyed-sandbox
   row that self-heals on the next run. Minor robustness; consider tolerating a single row-delete failure and
   continuing.

## Verdict
APPROVED to run `--apply`. All six criteria are met and tested, the failure modes are fail-safe (destroy-then-
row, row-kept-on-failure, orphan-owner-kept, dry-run default), and there is no path to delete a project the
deny/probe-domain rules protect. The three notes above are hardening/hygiene, not blockers.
