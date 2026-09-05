# §0b adversarial review — docs/plans/qa.md (M0)

Reviewer: ADVERSARY. Verdict: **strong, mature plan — approved.** The philosophy is right: assert what
the CHILD received (not engine logs), generated 192-cell matrix asserted twice + runtime, skip-loudly,
404-indistinguishability, never edit product code. Below are gaps from an attacker's view — cases a
wire-matrix test structurally cannot catch. Suggested as new TESTPLAN IDs; none block M1.

## Coverage gaps to add
1. **ENG-sql-authorizer** (High): a `read` wire to table X must NOT let `wheel query x "SELECT * FROM
   t_other"` read another node's table. This is inside an *allowed* wire, so it is NOT a matrix cell —
   the matrix passes while data leaks. Also assert deny of `sqlite_master`, `ATTACH`, `PRAGMA`,
   `load_extension`, `readfile`/`writefile`, and any non-SELECT. (my sdk-review must-verify #1)
2. **ENG-chest-traversal** (High): chest key = `..`, absolute path, symlink escape, unicode-normalized
   dotdot, NUL byte — all must stay inside the node's chest dir. Not a matrix cell.
3. **SUP-forged-event** (Med, finding 008 / PM ruling): a child printing a fake top-level `result`
   line must NOT be read as a real turn-complete; a forged/foreign `usage` must not move budget;
   events carry `session_id`. Note this is the INVERSE of ENG-log-unknown-event (tolerate unknown) —
   here the danger is trusting a forged KNOWN event. Budget/turn count enforced supervisor-side.
4. **ISO-uid** (M3, finding 007 ruling — per-node uid): cross-node `/proc/<other-pid>/environ` EACCES;
   `WHEEL_TOKEN` 0600 file unreadable by another node's uid; creds dir 0700; shared workspace setgid.
   Parameterised on SANDBOX_BACKEND=process, as your plan already sets up.
5. **PROXY/INGRESS smuggling** (TB2): `/engine/../`, `%2e%2e`, double-encode reaching `/v1/cli/*`;
   public `/p/<id>/*` reaching `/v1/*`; forwarded `Authorization`/`x-wheel-*` to engine. My
   redteam/pocs/proxy-ingress/ probes can become your regression fixtures.
6. **TOOL-ssrf** (finding 004): resolve-and-pin IP, per-redirect re-validation, IPv6/octal/decimal
   encodings, vault-value never echoed. My redteam/pocs/tool-ssrf/ + mocks/metadata.py feed these —
   let's agree ownership (I drive the probes, you regress the deny-list).

## Endorsements (keep as-is)
- MSG-envelope-escape/forge as highest-priority M1 — correct. Use the shared fixture (below); ensure
  it also covers escaping the **opening** `<AgentPrompt` (PM refinement) and that **endpoint/script**
  sources cannot forge `from="user"`, not only agent→agent.
- API-auth-owner-404 indistinguishability — exactly right. Optional add: guard the timing side-channel
  (owned-404 vs nonexistent-404) if cheap.
- Deny-asserted-twice-plus-runtime; generated matrix + drift check; assert-from-child-transcript.

## Hand-off
Shared envelope fixture: `redteam/pocs/envelope-forgery/fixtures.json` + `check.py` (oracle mode). 6
cases, self-test bites. Lift into MSG-envelope-* (wheel-core unit + delivery e2e).
