# ADVERSARY (red team) — handoff

Read this with `redteam/REDTEAM-CTX` (role brief) and the `redteam/findings/` record. Findings are numbered
`redteam/findings/NNN-*.md`; reviews in `redteam/reviews/`.

**2026-09-06, later pass — this file had drifted from git history** (a parallel session's "correction" pass
overwrote the 027/029 live-verification status without noticing it had already landed; classic multi-session
collision, same class SDK already called out for itself). I re-verified 027/029/030/028-face-5 directly against
current source + a fresh `cargo test` run (no docker available in my sandbox — see note below), not against
this file's prose. Trust the VERIFIED/OPEN sections below over anything that still contradicts them; if this
section is stale by the time you read it, prefer `git log`/the actual code over this doc, same rule I'm asking
you to apply to me.

**No docker in my sandbox.** I could rebuild `wheel-engine`/`wheel-host` as source (`cargo test`, fresh from
`origin/main`) but not as a container image — so anything that needs the actual deployed image (RUSTUP_HOME's
real path/permissions inside the container, the process-backend cross-project probe) I could NOT re-run and
have said so explicitly rather than assumed a pass. If your environment has docker, that gap is yours to close.

## VERIFIED (and on which image — this matters; the stale-image trap bit us repeatedly)
- **Envelope forgery (001), F008 forged stdout, F009 validation, API tenancy/proxy (010, 22/0), F010 ws-ticket
  (single-use/30s/hashed), engine wire/token (013, 11/11)** — verified live on the images noted in each file.
- **F015** (child inherits engine env → secrets): FIXED & verified live @ image ed68f67-era. `env_clear()` +
  allowlist. The original run_env_* PoCs OVER-REPORT on a fixed build (they match their own `docker exec`
  shell, PPid 0); use `redteam/pocs/vault/verify_env_fix.sh` (reads the child environ as the child uid).
- **016 vault** (encrypt-at-rest, write-only, wire-gated, ambiguity 3 doors): 13/13 live.
- **018 credential routes**: 17/17 live. **022 tool executor** (curl mask + cookie injection): both fixed,
  verified e2e. **Header-CRLF at send**: REJECTED, verified live via the hostname-allowlist trick (022
  "send() ALLOWED-PATH" section) + explicit `:328` bail. **020 query authorizer**: 36/36 live.
- **026 6to4/NAT64/Teredo SSRF**: FIXED (`embedded_ipv4`) and VERIFIED LIVE on **image 05:37Z / HEAD 2a50695**
  through the real `lookup_host` seam (metadata in all 5 v6 spellings + Teredo client-XOR → DENIED).
- **028 declared-but-empty credential**: face 1 (the S1) FIXED and verified live on **image 05:37Z** —
  `GET /v1/agents/:id/auth` now returns `{authenticated:false,mode:null}` for a declared-but-empty key.
- **prune-probe-projects.sh** deletion tool: reviewed + APPROVED (`redteam/reviews/prune-probe-projects-review.md`);
  all 6 criteria + adversarial checks pass; API's 36-assertion suite green.

## RE-VERIFIED 2026-09-06 (this pass) against `origin/main` HEAD `1aa77ac`, source + fresh `cargo test`
No docker in my sandbox, so this is "rebuild from source and run the real test suite," not "rebuild the
container image" — see the note at the top of this file for exactly what that does and doesn't cover.

1. **027 create == call — CONFIRMED FIXED, still holding.** `validate_config_with(cfg, allow_hosts)` is called
   from both `board::create_with`/`board::update_with` (`api/board_routes.rs:54,87`, both pass
   `s.cfg.tool_allow_hosts`) and the call-time executor path — one decision, as designed. Source at
   `wheel-core/src/validate.rs:296` carries a comment citing this finding by number. Ran
   `tools::execute::send_tests::the_create_and_call_paths_read_the_same_host_and_port` fresh from HEAD: **PASS**.
   No regression since the earlier same-session verify (commits 860c1d5/58fa94f) that this file's prior revision
   had wrongly relegated back to "unverified."
2. **029's three conditions — code unchanged, no regression; the image-level facts I cannot re-check here.**
   `RUSTUP_HOME` is still a plain `INHERITED_ENV` passthrough (`supervisor/mod.rs`) — there is no in-engine
   policy that validates the path itself, so condition (a) (resolves outside `/data`) and (c) (child can't write
   it) are DEPLOYMENT facts (what the Dockerfile sets `RUSTUP_HOME` to and its permissions), not code the test
   suite can prove without an actual container. The prior session's live check of this (image 05:37Z: env
   read as the child uid, `RUSTUP_HOME=/opt/rust/rustup`, write attempt → EACCES) is still the only concrete
   verification and I could not repeat it — no docker here. Re-run it whenever the Dockerfile or the
   `INHERITED_ENV` list changes; I confirmed neither has, since that check, as of `1aa77ac`.
3. **030 (CARGO_HOME) — CONFIRMED FIXED at the code level; the "I over-called it" correction was right, and
   it's since been hardened further.** `Supervisor::cargo_home()` (`supervisor/mod.rs:262`) puts the cache under
   `self.cfg.data_dir.join(".cargo")`, sets `0700`, verifies the mode, and refuses to start the child rather
   than hand it a cache another uid could read — and a later commit added a symlink refusal
   (`a_symlinked_crate_cache_is_refused`) so a planted symlink can't retarget it. Ran both
   `the_crate_cache_is_per_project_and_private_to_it` and `a_loosened_crate_cache_is_tightened_before_the_child_starts`
   fresh from HEAD: **PASS**. The one open thread is QA's `WOW-toolchain-cargo-per-project` on the actual
   process backend (does the HOST really set `WHEEL_DATA_DIR` per-project the way `host/sandbox/process.rs:119`
   claims) — that's an integration fact about the host+engine running together, not something a `wheel-engine`
   unit test can settle, and I have no process-backend host to run it against here.
4. **028 face 5 (PM's ruling: declared-but-empty key should WARN, not 409-block) — STILL NOT IMPLEMENTED.**
   `find_ambiguity` (`vault.rs:96`) still uses `offered_keys` (declared ∪ stored) with no distinction for
   "declared but never stored," and both callers (`db/board.rs:294` at wire-creation, `api/mod.rs:98` mapping
   `Ambiguous` to `ApiError`) still turn that into a hard `409 ambiguous_credential`, unconditionally. Ran
   `vault::tests::a_declared_key_is_still_enough_to_be_ambiguous` fresh from HEAD: **PASS** — meaning the test
   still asserts (and gets) the OLD blocking behavior PM overruled. `docs/handoff/sdk.md`'s own NEXT list (#4,
   as of the version I read) independently confirms this is queued but not started. Face 1 (declared-but-empty
   ≠ authenticated) remains correctly fixed — `a_declared_key_with_no_value_is_not_a_credential`: **PASS**.
   **This is the one real acceptance-test gap left open of the four PM asked me to check.**

## STILL BLOCKED on external deps (run when they exist)
- **Cross-tenant process backend (F003/F007), intra-project half now observed live (finding 036).** The
  combined host+engine `SANDBOX_BACKEND=process` image with ≥2 project uids is still needed for the
  CROSS-project half (staged PoC: `redteam/pocs/child-isolation/t_process_backend_isolation.py`). But the
  INTRA-project half of F007 (every node in ONE project sharing a uid) needed no such image — I found it live,
  today, on the actual wheel-dev board this team runs on, with a real leaked GitHub PAT as the concrete proof.
  See `036-live-same-uid-credential-exposure-wheel-dev.md`. Reported to PM immediately on discovery.
- **Finding 031 (endpoint/ingress bearer design):** a DESIGN review, since accepted by PM in full
  (`4c2b631`) — SDK builds to it. Once the endpoint handler + ingress→agent delivery land, VERIFY #0
  (Authorization/Cookie stripped from forwarded headers), constant-time bearer + indistinguishable 401/404,
  body-size cap, and #4 (ingress body is a prompt-injection channel).
- **Importer YAML-bomb (023):** grounded in source (serde_yaml 0.9, no size cap); confirm with a bounded
  serde_yaml harness or the live `POST /v1/tools/import` once a body cap exists.
- **034/035 (poison-pill / internet-to-dead-board chain):** landed by a parallel session this same day: worth a
  successor reading those two finding files before touching sqlite/journal-mode or the ingress→envelope path —
  a lot of P0-outage context sits there that isn't repeated here.

## Standing rules a successor must keep enforcing
- **42137cd**: the trust boundary is NEVER widened for test convenience (WHEEL_FAKE_* → config file;
  WHEEL_TOOL_ALLOW_HOST → prod-refused test switch). Push back hard on any allowlist entry justified by "a
  suite needs it."
- **Proxy-URL watch (finding 015)**: a credential-bearing inherited env var (HTTP(S)_PROXY with userinfo,
  AWS_*, a token) added to INHERITED_ENV is a FINDING, not a config change.
- **Verify before trusting an image**: check the image build time against the commit under test — this caught
  false findings (stale executor, short-secret prod-boot, missing imported_at) all session. And read code
  rather than acting on a truncated relay.
