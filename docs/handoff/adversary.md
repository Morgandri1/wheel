# ADVERSARY (red team) — handoff

Read this with `redteam/REDTEAM-CTX` (role brief) and the `redteam/findings/` record. This is the state at
stand-down on 2026-09-06. Findings are numbered `redteam/findings/NNN-*.md`; reviews in `redteam/reviews/`.

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

## UNVERIFIED — a successor must run these (do NOT trust the 05:37 image for them; rebuild first)
The 05:37Z image (HEAD 2a50695) predates the RUSTUP_HOME / 027-HostPolicy / 028 landings that came in at
**origin/main d206b95 (engine 58a333c)** and later. A pass on 05:37 proves nothing and a fail is a stale-image
artefact (the class of thing QA chased for an hour). **Rebuild from `origin/main >= 58a333c` first**, then:

1. **027 create == call (one decider).** `validate_config_with(cfg, allow_hosts)` now threads the engine
   allowlist into wheel-core so create and call share one URL parser/decision (a test caught an
   `[::1]:8080` IPv6-bracket create/call mismatch). VERIFY: with `WHEEL_TOOL_ALLOW_HOST=127.0.0.1:PORT`, a
   `127.0.0.1:PORT` tool is now CREATABLE and CALLABLE; a non-allowlisted internal target is refused at BOTH
   create and call (same wording); `[::1]:PORT` behaves identically at create and call.
   - Command: `cd /Users/metatron/wheel && make engine-image` (confirm image timestamp > 58a333c), then adapt
     `redteam/pocs/tool-exec/run_tool_allowed.sh` (it creates a `127.0.0.1:18080` tool — previously FATAL,
     should now succeed) and add the [::1] create/call consistency check.
2. **029's three conditions** (my allowlist DECISION, commit a2af2d5): (a) RUSTUP_HOME on INHERITED_ENV
   resolves inside the image's read-only toolchain dir and is REFUSED under `/data` or a project dir;
   (b) CARGO_HOME is per-project + `0700` (see finding 030 — currently NOT); (c) the agent uid CANNOT write
   the shared toolchain dir (SDK claims this — verify: as the child uid, attempt a write into RUSTUP_HOME →
   EACCES). Rebuild first; RUSTUP_HOME landed after 05:37.
3. **028 face 5 (PM overruled SDK 409→warning):** a declared-but-empty key STILL 409-blocks wiring a second
   vault that holds the real value (SDK kept declared-key ambiguity; PM overruled to warning-not-409). The
   acceptance test is `redteam/pocs/vault/run_declared_empty.sh` section 5 — it must become a WARNING (wire
   created), not a 409. Currently red (409) on the 05:37 image; re-check after SDK implements the overrule.

## STILL BLOCKED on external deps (run when they exist)
- **Cross-tenant process backend (F003/F007):** needs a COMBINED host+engine RUNTIME image in
  `SANDBOX_BACKEND=process` with ≥2 project uids sharing `/data`. Staged PoC:
  `redteam/pocs/child-isolation/t_process_backend_isolation.py`. Ask API/QA for `WHEEL_HOST_CONTAINER` + two
  projects, or the combined image.
- **Finding 030 (CARGO_HOME) — I OVER-CALLED IT; now Low/needs-resolution.** The host sets `WHEEL_DATA_DIR`
  per-project in process mode (`host/sandbox/process.rs:119`), so `cargo_home = /data/projects/<id>/cargo` is
  per-project, NOT the shared `/data/cargo` I assumed. See 030's CORRECTION section. The ONLY open question:
  why is QA's `WOW-toolchain-cargo-per-project` test RED given process.rs:119 (test-setup gap vs a real
  deployment path)? Resolve on the process backend; do not treat as a confirmed cross-tenant leak.
- **Finding 031 (endpoint/ingress bearer design):** a DESIGN review — SDK builds to it. Once the endpoint
  handler + ingress→agent delivery land, VERIFY #0 (Authorization/Cookie stripped from forwarded headers),
  constant-time bearer + indistinguishable 401/404, body-size cap, and #4 (ingress body is a prompt-injection
  channel — the envelope is attribution-only; the blast radius is the internet-reachable agent's wire set).
- **Importer YAML-bomb (023):** grounded in source (serde_yaml 0.9, no size cap); confirm with a bounded
  serde_yaml harness or the live `POST /v1/tools/import` once a body cap exists.

## Standing rules a successor must keep enforcing
- **42137cd**: the trust boundary is NEVER widened for test convenience (WHEEL_FAKE_* → config file;
  WHEEL_TOOL_ALLOW_HOST → prod-refused test switch). Push back hard on any allowlist entry justified by "a
  suite needs it."
- **Proxy-URL watch (finding 015)**: a credential-bearing inherited env var (HTTP(S)_PROXY with userinfo,
  AWS_*, a token) added to INHERITED_ENV is a FINDING, not a config change.
- **Verify before trusting an image**: check the image build time against the commit under test — this caught
  false findings (stale executor, short-secret prod-boot, missing imported_at) all session. And read code
  rather than acting on a truncated relay.
