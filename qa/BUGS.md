# Open bugs

Filed by QA (and ADVERSARY) against `docs/TESTPLAN.md` IDs. Severity: **S1** data loss / security /
privilege escalation · **S2** spec violation · **S3** wrong but workaroundable · **S4** polish.

A bug is closed only when its TESTPLAN ID goes green — not when someone says it's fixed.

| # | TESTPLAN | Sev | Owner | Status | Title |
|---|----------|-----|-------|--------|-------|
| 001 | `NODE-config-unknown-key`, `NODE-endpoint-path`, `NODE-mcp-transport`, `NODE-script-lang`, `NODE-state-not-config`, `NODE-vault-writeonly`, `NODE-endpoint-auth` | ~~S2~~ **S3** | SDK | ~~closed~~ | Exported JSON Schema accepts 12 configs the contract forbids |
| 002 | `NODE-type-closed` | S3 | SDK | ~~closed~~ | `node-config` union falls through to the `script` branch for an unknown type instead of failing |
| 003 | `NODE-tool-config` | **S2** | SDK | ~~closed~~ | `ToolConfig` diverges from §3d: no `kind`, and `source{format,raw,imported_at}` flattened to `source_format` |
| 005 | `make check` (`rust:fmt`) | S3 | API | ~~closed~~ | `main` fails `cargo fmt --check`: 66 diffs across 18 files in wheel-api + wheel-host |
| 004 | `WM-export-conformance`, `WM-endpoint-vault-read`, `WM-script-tool-read` | S3 | SDK | ~~closed~~ | `wire_allowed` was missing two contract rows: `endpoint→vault (read)` and `script→tool (read)` |
| 007 | `E2E-landing` | S3 | Web | **open** | Landing page hydration mismatch: `WheelMark` trig coordinates differ between Node and browser V8 |
| 006 | `PERF-check-budget`, §0b | **S2** | SDK + API | **open** | §0b 90%-per-crate gate: 4 crates below the bar (latest main: wheel-api 89.02%, wheel-cli 0.00%, wheel-core 70.51%, wheel-host 68.33%) |
| 009 | `ENG-log-stream-parity`, `COMMS-observability` | **S2** | SDK | ~~closed~~ | `transcript` log lines are persisted but never emitted over the events WebSocket |
| 010 | `ENG-image-contents`, `CLI-*` | **S1** | SDK | ~~closed~~ | The `wheel` CLI is absent from the engine image — agents have no interface to the board |
| 011 | `ENG-one-process`, `ENG-park-resume`, `MSG-delivered-means-delivered` | **S1** | SDK | ~~closed~~ | After any failed start, every later start was a silent no-op — and a turn could be written to a dead child's stdin and marked delivered |
| 012 | `make check` (`web:test`) | ~~S4~~ **S3** | Web | **open** | 30 local-auth vitest cases fail on node ≥ 22.4: Node's own experimental `localStorage` global shadows jsdom's |
| 013 | all `qa/integration/*` IDs | **S2** | QA | ~~closed~~ | QA's own integration suite was `if: false` in CI and had never run there — 127 assertions passed only on one laptop |
| 014 | `API-lifecycle`, `API-project-crud` | S3 | API | **open** | `infra/docker-compose.yml` defaults `ENGINE_IMAGE` to `wheel-engine:stub`, an image nobody builds — every project start 500s on a checked-out stack |
| 015 | `make check` (`rust:clippy`), `BACK-docker-backend` | **S2** | API | **open** | `wheel-host` does not compile on Linux: `RLIMIT_*` cast to `u32` where the target wants `c_int` — the dev stack cannot build |
| 015 | `make check` (`rust:test`), `API-lifecycle` | **S2** | API | ~~closed~~ | `wheel-host` did not compile on Linux: `as_pairs` returned `(u32, _)` where `c_int` was expected |
| 016 | `make check` (`rust:clippy`, `rust:test`) | **S2** | API | **open** | The fix for 015 is itself a Linux lint error: `as u32` is required on macOS, redundant on glibc, and `-D warnings` makes redundant fatal |

---

## 001 — Exported JSON Schema accepts 12 documented-invalid node configs · S3 (downgraded from S2) · SDK

**DOWNGRADED 2026-09-05 on evidence, not opinion.** The open question was never "is the schema
loose" — it plainly is — but "does anything else reject these?", which decided whether this was a
documentation defect or a hole. It is now answered: `qa/integration/test_engine_validation.py`
posts all twelve to a live engine and all twelve are rejected (422 serde `deny_unknown_fields`
or 400 `validate.rs`). ADVERSARY found the same independently (findings 009, 013); their probe is
now a permanent regression rather than a one-off observation.

The suite also asserts the engine ACCEPTS the valid fixtures, because an engine that rejected
everything would otherwise score a perfect 12/12 and look secure.

Still worth fixing: the schema is published as the contract's machine-readable form, so a client
generating types from it will build shapes the engine refuses, and will find out at runtime. That
is a real defect — it is just not a missing defence.

`docs/schema/*.json` is more permissive than `ARCHITECTURE.md` §3. Each fixture below is rejected
by the contract in prose but **accepted** by the schema, so the schema cannot be used as the
validation gate it's meant to be.

**Repro:** `make check` (gate `qa:contract-schema`), or:
```
source qa/.venv/bin/activate && python3 qa/contract/schema_fixtures.py
```

| Fixture | Expected | Actual | TESTPLAN |
|---|---|---|---|
| `invalid/config_unknown_key` | reject unknown key in `config` | accepted | `NODE-config-unknown-key` |
| `invalid/state_in_config` | runtime `state` fields not accepted as config | accepted | `NODE-state-not-config` |
| `invalid/vault_with_values` | vault config carries key NAMES only | accepted | `NODE-vault-writeonly` |
| `invalid/endpoint_no_slash` | `path` must lead with `/` | accepted | `NODE-endpoint-path` |
| `invalid/endpoint_traversal` | `path` must not contain `..` | accepted | `NODE-endpoint-path` |
| `invalid/endpoint_auth_bad_mode` | `auth.mode ∈ {none,bearer}` | accepted | `NODE-endpoint-auth` |
| `invalid/endpoint_auth_bearer_no_ref` | `bearer` requires `vault_ref` | accepted | `NODE-endpoint-auth` |
| `invalid/mcp_stdio_no_command` | `stdio` requires `command` | accepted | `NODE-mcp-transport` |
| `invalid/mcp_http_no_url` | `http` requires `url` | accepted | `NODE-mcp-transport` |
| `invalid/mcp_both` | not both `command` and `url` | accepted | `NODE-mcp-transport` |
| `invalid/script_zero_timeout` | `timeout_secs` > 0 | accepted | `NODE-script-lang` |
| `invalid/script_timeout_over_max` | `timeout_secs` ≤ 300 | accepted | `NODE-script-lang` |

**All twelve are expressible in JSON Schema** — `additionalProperties: false` on each config
variant, `pattern` on `endpoint.path`, `oneOf` + `required` for the mcp transport variants,
`minimum`/`maximum` on `timeout_secs`, and omitting state fields from the config schema. This is
not a "JSON Schema can't express it" case.

**Why S2 rather than S4:** the schema is what Web generates its types from and what the engine is
meant to validate against. If it accepts `..` in an endpoint path and vault values in a config, the
two independent defences the contract asks for (§ "rejected at creation time by engine AND api")
collapse into one, and `SEC-chest-traversal`/`ING-traversal` lose their static half.

**Not a bug, noted:** the 6 `valid/tool_*` fixtures also fail, because the `tool` node type isn't in
the schema yet. That's expected pending SDK's tool commit; those fixtures are already written and
will go green on their own. No action beyond landing the type.

## 002 — `node-config` union falls through to `script` for an unknown type · S3 · SDK

**Repro:** validate `qa/fixtures/nodes/valid/tool_source_manual.json` against `docs/schema/node.json`.

**Actual:** `{'format': 'manual', 'raw': ..., 'imported_at': ...} is not of type 'string'` at
`config.source` — the validator matched the **`script`** branch (whose `source` is a string) and
reported a confusing type error deep inside it.

**Expected:** an unknown/unhandled `type` fails as an unknown node type (`NODE-type-closed`), not
as a field-level error inside an unrelated branch.

**Why it matters beyond cosmetics:** a tagged union that silently matches the wrong branch can
*accept* a malformed node whose fields happen to line up with a sibling variant. Discriminate on
`type` (`if/then` per variant, or `oneOf` with a `const` tag) so each config can only ever be
checked against its own branch.

## Closed

*(none yet)*


---

## 003 — `ToolConfig` diverges from the §3d contract · S2 · SDK

`crates/wheel-core`'s `ToolConfig` (as exported to `docs/schema/tool-config.json`) does not match
`ARCHITECTURE.md` §3d. This is not a schema-strictness issue like 001 — two contract fields are
absent from the type altogether.

**Repro:** `make check` (gate `qa:contract-schema`), or
`qa/.venv/bin/python qa/contract/schema_fixtures.py`. Fails on
`invalid/tool_bad_kind` and `invalid/tool_bad_source_format`, both ACCEPTED.

| Contract §3d | Implemented | Consequence |
|---|---|---|
| `kind: "http"` (and `"email"` per §3e) | **field absent** | A tool node can't declare what kind it is. §3e's email tool node has nowhere to live, and `tool_bad_kind` (`kind: "grpc"`) is accepted because the key is simply ignored. |
| `source: { format, raw, imported_at }` | flat `source_format` (nullable); `raw` and `imported_at` absent | **§3d rule (5) becomes unimplementable**: "re-import diffs operations by `method+path`, keeps existing fills". You cannot diff against the previous spec if the previous spec was never stored. `imported_at` is also how the UI shows staleness. |

Both invalid fixtures are accepted only because the schema has no `additionalProperties: false` —
the unknown `kind` / `source` keys are silently dropped. So a client that writes the contract's
shape gets a 200 and loses the data, which is worse than a 400.

**Expected:** `kind: "http" | "email"` required, and `source` as the nested object §3d specifies,
retaining `raw` so re-import can diff.

**Note on how this stayed hidden:** `make check` reported green on `main` because the
`qa:contract-schema` gate SKIPS when `jsonschema` isn't installed, and `qa/.venv` is gitignored.
The gate was correct to skip rather than fail, but a skipped gate on `main` is a blind spot — so
CI now runs `make bootstrap` first, and `make check-strict` (CHECK_STRICT=1) treats any skip as a
failure. Fixed as part of this report.


---

## 004 — Exported wire matrix is missing two contract rows · S3 · SDK

Reported by Web for `endpoint→vault (read)`. QA's independently-derived matrix finds **two**
missing rows, so the corrected allowed count is **26**, not 25.

| Row | ARCHITECTURE.md §3 | `wire_allowed` / export |
|---|---|---|
| `endpoint → vault (read)` | line 160 — "resolve the endpoint's `auth.vault_ref` bearer secret" | absent |
| `script → tool (read)` | line 157 — "script → tool \| same as agent" | absent |

**Repro:** `make check` (gate `qa:wire-conformance`), or:
```
qa/.venv/bin/python qa/contract/wire_matrix_conformance.py
```
```
contract (QA, from §3 prose): 26 allowed
export   (wheel-core):        24 allowed
```

**Expected:** `wire_allowed(Endpoint, Vault, Read)` and `wire_allowed(Script, Tool, Read)` both
true; `docs/schema/wire-matrix.json` carries 26 triples.
**Actual:** both false and absent, so neither wire can be created by engine, API or UI.

Only the ALLOWABILITY lands now (two rows plus tests). Bearer-auth behaviour and tool execution
remain M2 per the milestone plan.

**Note:** when SDK regenerates, Web's `wire-matrix.conformance.test.ts` goes red until they re-run
`pnpm gen:types` and add plain-language strings for the two new rules. That is intended, and Web
has been told to expect it.


---

## 005 — `main` is red on `rust:fmt`, blocking all merges · S3 (blocking) · API

`cargo fmt --all -- --check` fails on `main` with **66 diffs across 18 files**, all in
`crates/wheel-api` and `crates/wheel-host`.

**Repro:** `make check` on a clean `main`, or `cargo fmt --all -- --check`.

**Fix:** `cargo fmt --all` — seconds. No code change, no review needed.

Files: `wheel-api/src/{lib,config,crypto,error,models,orchestrator,state}.rs`,
`wheel-api/src/auth/claims.rs`, `wheel-api/src/http/hop.rs`,
`wheel-api/src/routes/{ingress,projects,proxy}.rs`,
`wheel-api/tests/{auth_verify,config_interlock}.rs`,
`wheel-host/src/{main,proxy,store}.rs`, `wheel-host/src/sandbox/docker.rs`.

Not a correctness defect, but it is the merge gate, so while it is red nobody can merge anything
per §1 of the contract. Filed as blocking for that reason alone.


---

## 006 — Per-crate coverage below the §0b bar; `wheel-host` at zero · S2 · SDK

> **2026-09-06 03:39Z, CI run 34009465273 on `main`** — the only red gate in `make check`
> is `rust:coverage`, and only two crates are now under the bar: **`wheel-cli` 86.88 %**
> (was 0.00 % — SDK's `ce3bdc8` covered the transport and command dispatch) and
> **`wheel-host` 76.94 %**. `wheel-core` and `wheel-api` pass. `rust:clippy` and
> `rust:test` are GREEN, which closes 016.

> **2026-09-06 — THE NUMBERS BELOW ARE SUSPECT AND ARE BEING RE-MEASURED.**
> `make coverage` ran `cargo llvm-cov` against the shared `target-dir` that every worktree
> uses, and summed per-crate lines with a filter that accepted any path containing
> `/crates/` — so one crate's coverage was totalled across six checkouts of it. `validate.rs`
> read 0% while it was actually 97%, which dragged `wheel-core` to 4.85%. Fixed in
> `qa/tools/coverage_gate.py` (private `CARGO_TARGET_DIR`, files scoped to this worktree, and
> an empty result is a SKIP rather than 0%). CI was never affected — one checkout, one target
> dir. Re-publishing the table after a clean local run.

Latest numbers, CI run 33961539782 (bar 90%, per crate). `wheel-api` has gone 35% -> 89.02%:

| crate | lines | status | owner |
|---|---|---|---|
| `wheel-api` | 89.02% (1329/1493) | FAIL — within 1 point of the bar | API |
| `wheel-core` | 69.56% (681/979) | FAIL | SDK |
| `wheel-host` | 70.69% (521/737) | FAIL | API |
| `wheel-engine` | 57.02% (1194/2094) | EXEMPT — scaffolding (PM ruling); expires when the engine is bootable / `wheel-engine:test` exists |

`wheel-host` was at **0.00%** when this was filed and is now at 72.21% — that was the urgent one
(it holds every project's engine secret, performs the setuid, and is the only process touching the
docker socket), so the risk has dropped a lot. All three are still below the bar.

**The gate itself was broken until now, and this is worth knowing:** `coverage_gate.py` wrote its
llvm-cov report to `<repo>/target/`, but `~/.cargo/config.toml` points every worktree at one
shared target-dir, so that directory does not exist and `llvm-cov` exited with "failed to create
file". Every "coverage" result before this fix was that error surfacing as a red gate, not a
measurement. Fixed to use a temp dir. So treat the numbers above as the first trustworthy ones.

`wheel-core` at 68% matters for a specific reason: `validate.rs` is the code that must be
enforcing the twelve rejections in BUG-001 that the exported schema does not. Untested branches
there are the difference between "the schema is loose but the engine catches it" and "nothing
catches it". Those twelve are marked `_enforced_by: engine` in the fixtures and will be asserted
against a real engine once `wheel-engine:test` exists; coverage is the cheaper earlier signal.

**Repro:** `make coverage`, or CI's `make check-strict`.

**Note on the exemption:** it is declared in `qa/tools/coverage_gate.py` naming the crate, the
reason and the event that expires it. If `wheel-engine` reaches 90% while still exempt, the gate
FAILS and tells us to remove the exemption — a stale exemption cannot outlive its reason.

---

## 007 — Landing page hydration mismatch from trig floating-point · S3 · Web

`E2E-landing` fails: 48 console errors, all from one React hydration mismatch.

**Cause** (from the CI trace, `web/src/components/header.tsx`, `WheelMark`): the SVG spoke
coordinates are computed with `Math.cos`/`Math.sin`, and the result differs in the last digit
between the Node server renderer and browser V8:

```
+  y2={3.9892650149939453}     (client)
-  y2="3.9892650149939435"     (server)
```

React serialises the server value as a string and compares; the two differ, so the whole tree is
flagged as "hydrated but some attributes did not match. This won't be patched up."

Trig is not required to be bit-identical across JS engines, so this will reproduce on any
server/client pair, intermittently by platform — the kind of bug that is nearly impossible to find
by looking, and easy to fix once seen.

**Fix:** round to a fixed precision so both sides emit the same string, e.g.
`const p = (n: number) => n.toFixed(3);` and use `x1={p(...)}`. Three decimals is far finer than
a 24-unit viewBox can show.

**Why it is worth fixing rather than muting:** "this won't be patched up" means React keeps the
server DOM for that subtree, so any client-side state it depends on can silently diverge. It is
also 48 console errors on the first page every visitor sees, which buries real errors.

**Repro:** CI e2e job, or `make test-e2e` on a machine that can launch chromium.

---

## CLOSED

**003 · ToolConfig §3d divergence** — closed by SDK. `qa:contract-schema` now rejects both
`tool_bad_kind` and `tool_bad_source_format`. Verified by the gate itself: the fixtures were
tagged `_known_bug: BUG-003`, and a tracked gap that starts PASSING fails the build, so the fix
announced itself rather than waiting for someone to notice.

**004 · wire matrix missing two rows** — closed by SDK. `qa:wire-conformance` now reports
contract 26 / export 26. Same mechanism: the `KNOWN_GAPS` entries failed once the rows appeared.
Reported by Web (`endpoint→vault`), second row (`script→tool`) found by QA's contract-derived
matrix.


## 001 — RESOLVED, and downgraded S2 → S3 on evidence

BUG-001 said the exported schema accepts 12 configs the contract forbids. That was true, but it
was only half a finding: it established that ONE of two defences was loose, and said nothing
about whether the other held. I had marked all 12 fixtures `_enforced_by: engine` and deferred
the question until a real engine existed.

`wheel-engine:test` now exists, so the question is answered rather than argued:
**`qa/integration/test_engine_validation.py` POSTs all 12 to a live engine and all 12 are
rejected** (35/35 green, including that all 20 valid fixtures are still accepted — a validator
that rejects everything would also have "passed" the negative half).

| Fixture | Engine |
|---|---|
| `config_unknown_key`, `state_in_config`, `vault_with_values` | rejected |
| `endpoint_no_slash`, `endpoint_traversal`, `endpoint_auth_bad_mode`, `endpoint_auth_bearer_no_ref` | rejected |
| `mcp_stdio_no_command`, `mcp_http_no_url`, `mcp_both` | rejected |
| `script_zero_timeout`, `script_timeout_over_max` | rejected |

So the engine — the authority — enforces the contract correctly, and no invalid config can be
persisted. What remains is that the *published schema* is more permissive than the engine, which
is a real defect for anyone generating a client from it (they will build a request the engine
then refuses), but it is not a security hole and no defence has collapsed. **S3, closed.**

ADVERSARY: please do not cite this as "defence in depth reduced to one layer" — it is one layer,
and that layer holds, with a test to prove it. The accurate version is: the schema is advisory,
the engine is authoritative, and the two disagree about strictness.

## 002 — closed. `qa:contract-schema` now rejects `invalid/type_unknown`.

## 005 — closed. `rust:fmt` is green on main; `make check` is 13/13.


---

## 001 — DOWNGRADED S2 -> S3 on evidence

The engine DOES reject all twelve configs the exported schema accepts. Verified against a real
`wheel-engine:test` container: `qa/integration/test_engine_validation.py`, 35/35 green, including
`NODE-engine-rejects/*` for every one of the twelve.

This resolves the open defence-in-depth question. The concern was that BUG-001 collapsed
"rejected by engine AND api" to a single layer, leaving the surviving layer unverified. It is now
verified, and it holds. So this is a published-contract defect — `docs/schema/*.json` is the
artifact Web generates types from and third parties would validate against — not a security hole.

Still worth fixing, for a reason the severity change should not obscure: a client that writes a
config the schema calls valid gets a 400 from the engine. The schema currently promises something
the product does not accept, which is a worse failure than a schema that is merely strict.




---

## 009 — `transcript` log lines never reach the events WebSocket · S2 · SDK

The engine persists `stream: "transcript"` log lines to the database but does not emit them on
`GET /v1/events`. A WS consumer sees only `stdout`.

**Repro** (needs a live stack — this is the check that found it):
```
node qa/live/ws_streams_parity.mjs
```
```
  WS log streams : stdout
  DB log streams : transcript, stdout
  FAIL every DB stream also arrived over the WS — missing over WS: transcript
```

**Expected:** every stream persisted to the DB also arrives over the WS, so a live consumer and a
replay of `GET /v1/agents/:id/log` agree.
**Actual:** `transcript` is DB-only.

**Why S2 rather than cosmetic:** §3c #10 requires Web's agent drawer to show "the exact bytes
written to a child's stdin". That is the `transcript` stream. As it stands Web cannot render it
live — it would have to poll the log endpoint and reconcile against the WS, which is the kind of
divergence that produces a UI that is quietly wrong rather than obviously broken. It also weakens
`ENG-events-replay`: WS and replay are supposed to be two views of one log.

**Provenance:** adapted from SDK's own `ws-live2.mjs` probe, with the deliberate change that this
version ASSERTS and exits non-zero instead of printing the two sides for a human to compare.
SDK's e2e passed this bug because it asserted that *a* log event arrived rather than WHICH streams
did — a printer cannot fail, so it cannot gate. SDK asked for exactly this check to live in `qa/`.


---

## 010 — The `wheel` CLI is not in the engine image · S1 · SDK

**CLOSED** — verified against `wheel-engine:dev`/`:test` rebuilt at 10:59Z from c99ed40:
`wheel`, `wheel-engine`, `wheel-host` and `python3` are all on PATH and `wheel --help` prints.
The standing regression is `qa:image-contents` in `make check` plus the `Verify image contents`
step in the docker-sandbox CI job, so a build that drops a required binary now fails instead of
shipping green.

`wheel-engine:test` (and `:dev`, same Dockerfile) contains `wheel-engine`, `wheel-host`,
`claude`, `codex` and `python3` — but **no `wheel` binary**. Every CLI test fails with
`exec: "wheel": executable file not found in $PATH`.

**Repro:**
```
docker run --rm --entrypoint sh wheel-engine:test -c 'command -v wheel || echo MISSING'
python3 qa/contract/image_contents.py     # new gate, fails on exactly this
python3 qa/integration/test_engine_cli.py # 17 failed, 4 passed
```

**Root cause — two silent-failure layers in `docker/Dockerfile.host`.** The crate is
`wheel-cli`, but its binary is named `wheel` (`[[bin]] name = "wheel"`):

```dockerfile
# line 20 — there is no binary called wheel-cli, so this fails, and `|| true` swallows it
&& (cargo build --release --bin wheel-cli 2>/dev/null || true)

# line 50 — the [i] glob means "copy only if it exists"; /src/target/release/wheel-cli
# never exists, so nothing is copied and the build still succeeds
COPY --from=build /src/target/release/wheel-cl[i] /usr/local/bin/
```

**Fix (two lines):**
```dockerfile
&& cargo build --release --bin wheel \
COPY --from=build /src/target/release/wheel /usr/local/bin/wheel
```

**Why S1:** §3 makes the wire set a node's capability set, and the CLI is how a node exercises
it — `msg`, `read`, `write`, `inbox`, `secret get`, `run`. With no binary, an agent cannot reach
the board at all. The vertical slice's "`wheel msg` between two agents works" is unachievable as
shipped, so this blocks M1.

**Process point, which is the more useful half:** nothing failed. `|| true` and the optional-glob
`COPY` are each individually reasonable — they exist so the image can build before every crate
lands — but together they turned "the agent's entire interface is missing" into a successful
build. This is the same shape as the silent test skips API and I have both been removing today.
`qa/contract/image_contents.py` now asserts the image's contents explicitly, so the next missing
binary is a red gate rather than a discovery three hours later.


---

## 011 — After a failed start, every later start was a silent no-op · S1 · SDK · CLOSED

**Found and fixed by SDK, not by QA.** Recorded here because §3c #15 makes this file the
system of record and a message is only a notification. Fixed in `0c8341f`, on `main` at
`158500f`.

**Repro (pre-fix):**
```
1. create an agent node with no credentials stored
2. POST /v1/agents/:id/start            -> child dies, status needs_auth   (correct)
3. POST /v1/agents/:id/auth/complete {"api_key": "sk-ant-api03-…"}
4. POST /v1/agents/:id/start            -> 200 OK, {"status":"stopped"}, and NO process
```
Expected a new child. Actual: nothing, forever; an engine restart was the only recovery.

**Cause:** nothing cleared an agent's supervisor slot when its child exited, so `start` took
the "already running" early return for a process that no longer existed. The same root cause
let `pump_queue` write a turn into a dead child's stdin and mark it `delivered` — §3c #15
inverted, since `delivered` is defined as "the bytes reached the child's stdin".

**Fix:** one owner for a child's death — clear the slot, reap, requeue in-flight messages,
revoke the node token, record why. A silent exit is now `stopped` rather than `error`, since
a clean shutdown looks identical from outside. Three supervisor tests against a stub binary,
including ten-starts-one-process for §3c #13.

**What QA takes from it.** My suites did not catch this, and the reason is worth naming: every
lifecycle test I had drove `start` from a *clean* state. The recovery path — start after a
failure — was never exercised, so a permanently wedged agent was invisible to the whole suite.
`ENG-start-after-failure` is now in TESTPLAN §5a and `test_engine_auth_routing.py` drives
exactly that sequence (auth → start → re-auth → start) on every run, because it has to store a
credential and respawn to observe routing at all.


---

## 012 — `web:test` fails on any node newer than CI's · S3 (raised from S4) · Web

**Raised to S3 2026-09-05.** Still not a product bug — the code is fine and CI is right —
but I filed it as polish and then watched it make my own `make check` red twice, and the
second time I nearly merged on the assumption that the red was known rather than reading it.
That is the actual cost: `make check` is the pre-merge gate for five agents, and a gate that
is red for a reason unrelated to your change trains everyone to merge past it. The failure
mode is not the 30 tests, it is the next real failure nobody looks at.

A gate whose verdict depends on the developer's node version is not a gate, and this one
costs whoever hits it the time to work out they are debugging their runtime rather than the
product. I lost that time today, which is why it is written down instead of remembered.

**Repro:** `pnpm -C web test` on node ≥ 22.4 (mine is v26.8.1).
30 failures in `src/lib/local-auth.test.tsx`, all `TypeError: Cannot read properties of
undefined (reading 'clear')` at `window.localStorage.clear()`, alongside node's own warning:
`ExperimentalWarning: localStorage is not available because --localstorage-file was not provided`.

**Cause:** node ≥ 22.4 defines a built-in experimental `globalThis.localStorage` that yields
`undefined` unless `--localstorage-file` is passed. vitest's jsdom environment populates
globals from the jsdom window but does not overwrite a key that already exists, so node's
broken getter wins over jsdom's working `Storage`. jsdom itself is fine — constructing a
`JSDOM` by hand gives a real `localStorage`. **Confirmed by bisecting the runtime, not the
code:** identical tree, node 22.13.0 → 116/116 pass; node 26.8.1 → 30 fail.

**Suggested fix (Web's call, one line):** a `setupFiles` entry in `web/vitest.config.ts` that
redefines `globalThis.localStorage` from the jsdom window when the global is not a `Storage`.
Pinning node in `.nvmrc` or `engines` helps the honest case but does not stop the gate lying
to someone who ignores it.

**Two things I fixed on my side rather than leaving to the next person:**
1. `qa/check.sh` **prepended** `/opt/homebrew/bin` to `PATH`, so a developer who deliberately
   selected node 22 to match CI silently got homebrew's node 26 anyway — the gate answered
   about a runtime nobody chose. Those entries are a fallback for finding cargo/pnpm, never
   an override, so they are appended now.
2. `make check` prints the running node major when it differs from the 22 that CI pins, and
   says to suspect the runtime first if a web gate is red locally and green on CI.

---

## 013 — The integration suite has never run in CI · S2 · QA (mine) · CLOSED

Filed against myself. The ledger is the system of record (§3c #15), and this is the same
failure I have filed against three other people this week, so it belongs here in the same form.

**Repro (before the fix):** `gh run view <any main run> --json jobs` →
`integration (docker + fake harness): skipped`, on every run since the workflow was written.

**Cause.** The job carried:
```yaml
    # Skipped until SDK lands the engine image; flipped on then.
    if: false
```
The image landed at 10:59Z. Nobody flipped it. I wrote the condition, I got the image, I ran
the suite by hand, I reported 127/127 green — and I never went back to the workflow.

**Impact.** For the whole M1 window, 127 assertions — all 243 wire-matrix cells against a real
engine, the byte-exactness fixture, the credential-routing suite — existed and passed only on
my laptop. CI reported green throughout, and that green meant less than everyone reading it
believed. In the GitHub UI a disabled job and a passing job render the same unless you open
the job list, which is what makes this worse than an ordinary red.

**Fix (main @ 08d9ad8).**
1. `integration` enabled, `backend: [docker]`. `process` is left out of the matrix rather than
   allowed to fail: a row that is red on every run is how people learn to stop reading the
   column. It joins at M3 with the backend it tests.
2. `qa/contract/ci_workflow_lint.py` now **fails** on any job with `if: false` that has no
   entry in `DISABLED_OK` naming the reason and the EVENT that re-enables it — the same
   discipline PM ruled for coverage exemptions. Verified the rule fires by disabling a job
   and watching the gate go red.

**The part worth keeping.** The lint *already* detected disabled jobs. It printed them under
`deliberately disabled:` as informational output, and I stopped reading the line. Informational
output is not a gate; if it were worth printing it was worth failing on. That is the same
lesson as exit 77 (a gate that cannot run must not look like a gate that passed) arriving from
a direction I had not covered: a gate that *reports* a problem without failing on it.

---

## 009 — CLOSED 2026-09-05 · verified by an automatic gate, not by a claim

`qa/integration/test_engine_events.py` opens `/v1/events`, starts an agent, sends a message,
and asserts the set of log streams the WS broadcasts is a superset of the set the database
recorded over the same window. `transcript` is asserted by name as well, because that is the
stream this bug was about. 9/9 green against `wheel-engine:test`.

**The set comparison is the point.** SDK's own note on this bug: their e2e "asserted that *a*
log event arrived rather than WHICH streams did", so a missing stream could not fail it. A
presence check cannot fail while anything at all arrives. The set version needs no list of
stream names to maintain: a new stream is covered the day it is added, and a stream that stops
being broadcast is red the same day.

**And it nearly produced a false report.** My first run said `recorded ['stdout','transcript']
but broadcast only []` — a clean S2 against SDK for a bug they had already fixed. The real
frame is `{type:"log", line:{stream, text, …}}` and I was reading `stream` from the top level,
so every frame yielded nothing. An empty set from a parse error is byte-identical to an empty
set from a missing feature. The suite now separates them: `ENG-events-log-readable` fails with
the offending frame printed when log frames arrive and none carries a stream, and says in
words that the suite is reading wrong rather than the engine broadcasting wrong. Third time
this shape has nearly cost SDK an afternoon on my mistake; it is the one class of error where
my tests are the least trustworthy part of the system.

---

## 014 — compose points at an image nobody builds · S3 · API

`infra/docker-compose.yml:37` — `ENGINE_IMAGE: ${ENGINE_IMAGE:-wheel-engine:stub}`, under a
comment reading "SDK's real engine owns the wheel-engine:dev tag. **Until their build lands**
this stack runs...". The build landed. `wheel-engine:dev` and `:test` exist; `:stub` does not
exist and no target produces it.

**Repro:** bring up `infra/docker-compose.yml` with `ENGINE_IMAGE` unset, create a project,
`POST /v1/projects/:id/start`.
**Actual:** API 500 `internal`; host logs `Docker responded with status code 404: No such
image: wheel-engine:stub`, then `504 {"last_error":"creating project container"}`. Local dev
cannot start a project at all with the checked-in defaults.

Worked around in `qa/integration/run.sh` (the suite pins `ENGINE_IMAGE=wheel-engine:test`
rather than inheriting anyone's default), so this is not blocking the gate. The default is
still wrong for anyone running compose by hand.

**Same shape as 013.** Both are placeholders whose expiry condition is recorded only in a
prose comment — "until their build lands", "flipped on then" — and prose does not expire.
013's fix was to make the lint fail on a disabled job with no machine-readable reason and
re-enabling event. This one wants the same discipline.

---

## The class these three share

014 (`:stub`), 013 (`if: false`), and the `R.check(id, True)` in the vault suite are one
failure wearing three coats: **a placeholder that reports success**. None of them fails.
The image 404 surfaced as an API 500 that looked like an API bug; the disabled job rendered
identically to a passing one; the unconditional check occupied a TESTPLAN ID so the criterion
read as covered. In every case the honest signal — "this is not real yet" — was written down
in a comment, where nothing enforces it.

The rule I am applying from here: a placeholder must fail, or carry its expiry somewhere a
program reads. A comment saying "until X lands" is a note to a person who will not be looking.

---

## 015 — `wheel-host` does not compile on Linux · S2 · API · CLOSED (87a99d4)

**Repro:** `docker compose -f infra/docker-compose.yml up -d --build` on any Linux target
(CI, or the `host` service's own `debian:bookworm-slim` builder — so this reproduces on a
mac too, inside the container).

```
crates/wheel-host/src/sandbox/process.rs:205
  expected `Vec<(i32, u64)>`, found `Vec<(u32, u64)>`
  = note: expected struct `Vec<(i32, _)>`
             found struct `Vec<(u32, _)>`
error: could not compile `wheel-host` (lib) due to 2 previous errors
target host: failed to solve: "cargo build -p wheel-host" exit code: 101
```

On the CI runner the same code compiles but `-D warnings` rejects it as
`clippy::unnecessary_cast`, so `make check` is red there and the build is hard-broken in the
compose builder. Two symptoms, one cause.

**Cause.** `RlimitDefaults::as_pairs` returns `Vec<(u32, libc::rlim_t)>` and casts each
`libc::RLIMIT_*` with `as u32`. On glibc/Linux those constants are already `__rlimit_resource_t`
(a `u32`), so the cast is redundant; where the signature is expected to be `c_int` it is the
wrong type outright.

**The fix is already written in the file — it just is not the code.** Immediately above,
`process.rs:165` defines `RlimitResource` (`__rlimit_resource_t` on glibc/Linux, `c_int`
elsewhere) with a doc comment explaining this exact failure, and `as_pairs`'s own comment says
"The call site casts with `as _` so both targets are satisfied." The call site casts with
`as u32`. So the comment documents a fix that was not applied, and the alias that exists to
prevent this is unused at the one place it was written for.

**Why S2, not S3.** It blocks the whole local stack (`make dev`, `make test-int`) and holds
`make check` red on `main` for everyone, on top of BUG-006. Worth noting that this is the
second time this file has broken this way — its own comment records the first — which is an
argument for a Linux build in the pre-merge gate rather than only in CI.

**Not worked around.** Unlike 013 and 014 there is nothing for QA to pin: the binary does not
build. The integration gate is red until this lands.

---

## 016 — `main` is red on `rust:clippy`, Linux only · S2 · API · CLOSED

> Closed 2026-09-06: CI run 34009465273 on `main` has `rust:clippy` green on
> Linux; `rust:coverage` is the only remaining red gate. Verified from the run
> log, not from a fix notification.

`crates/wheel-host/src/sandbox/process.rs`, `Rlimits::as_pairs`: `libc::RLIMIT_NPROC as u32`
and five siblings. On glibc `RLIMIT_NPROC` is already `u32`, so clippy fires
`unnecessary_cast` — fatal under `-D warnings`. On macOS the same cast is `c_int -> u32` and
required. Fix: `as _`, which infers correctly on both and clippy does not flag.

Second failure in a row from the same three lines, and the direct result of the first:
BUG-015 was an E0308 on Linux, 87a99d4 fixed it by adding these casts, and the casts are
themselves a Linux lint error. Two bugs, one root cause — a platform-shaped gate.

**The finding underneath the bug, which is the one worth acting on.** All six of us develop on
one macOS host, so `make check` locally cannot catch a Linux-only break — by construction, not
by oversight. Both of these reached `main` with a green local gate, and the first shipped a
host image with no host binary in it. Our pre-merge gate is macOS-shaped and production is
Linux, so for anything touching `libc`, `#[cfg]`, or the container images, "green locally"
carries no information at all.

Proposed to API (their crate pays for it, so their call): either a `cargo check --target
x86_64-unknown-linux-gnu` step in `make check` that exits 77 where the target is not
installed, or a Linux `cargo clippy` folded into the `docker-sandbox` job, which already has
a Linux container and would return the verdict in ~2 minutes instead of at CI time.

This is BUG-013's lesson in a different key. There the gate was disabled; here the gate runs
faithfully and is measuring the wrong platform. Both produce the same artefact: a green check
that means less than the person reading it believes.

---

## 017 — `SEC-vault-at-rest` grepped an empty file · S2 · QA (mine) · CLOSED

The at-rest check read `/data/wheel.db` and asserted the canary was absent. The engine runs
sqlite in WAL mode, so on a short-lived test container `wheel.db` is a **4096-byte header**
and every row lives in `wheel.db-wal`. The suite was scanning nothing and reporting the
strongest claim it makes: "your secrets are encrypted at rest".

It would have passed against an engine that stored every vault value in plaintext.

Caught by its own positive control (`SEC-vault-at-rest/grep-works`), which asserts a value we
know is there IS findable by the same scan — it went red while the security assertion above it
went green. Fixed: scan `wheel.db`, `-wal` and `-shm`, and control on a ctx markdown stored
deliberately in the clear rather than on a key name. The control now *gates* the verdict: if
the scan cannot find the plaintext control, the at-rest criterion reports **skipped**, because
a broken search has no verdict to give.

## 018 — `qa:id-traceability` ignored any ID containing `%` · S2 · QA (mine) · CLOSED

The gate exists so that every ID a suite asserts is named in TESTPLAN. It skipped labels
containing `%`, reasoning that format placeholders are prose. True for `WM-setup/%s`, where the
parent carries the criterion. False for `SEC-child-env-no-%s`, which assembles the ID **body**:
two S1 criteria — the control-plane bearer and the vault key must not reach an agent child —
were asserted under IDs that appear nowhere in the plan. The gate reported "every asserted ID
is in the plan" and a reader of the plan saw no gap. Both were right and the criteria were
untraced.

Fixed: interpolating after a `/` stays legal, interpolating into the ID body fails with the fix
named. Verified by reintroducing the interpolated form and watching it go red.

## 019 — Integration suites collided on ports, env vars and container names · S3 · QA (mine) · CLOSED

Three suites defaulted to port 17413 and two to 17414; two different suites read
`WHEEL_ENGINE_PORT` with **different** defaults, so setting it to relocate one silently moved
the other onto a third. Nothing had failed yet — `run.sh` is serial — which is exactly why it
was worth fixing before it produced a 2am flake, or worse, one suite asserting confidently
about another suite's engine.

Then the real version arrived: a second QA session on this shared host ran the same suite at the
same time, its `docker rm -f` destroyed my wheel-on-wheel engine mid-clone, and the retry could
not start because it still held the port. Suite-level uniqueness does not help when the same
suite runs twice.

Fixed in two layers: `qa/contract/suite_isolation.py` (in `make check`) forbids shared default
ports, shared port env vars and shared container names across suites; and long-running suites
take a per-run container name plus `wheel_client.free_port()`, which falls back to an
OS-assigned port when the default is busy. Verified by running two engines side by side.

## 017 — Suites can test an image another agent replaced mid-run · S2 · QA (mine) · CLOSED

Six agents share one docker daemon, and at least SDK and I both build `wheel-engine:test`.
A tag is a mutable pointer: a suite that runs `docker run wheel-engine:test` twenty times over
four minutes can test containers from more than one build, and cannot tell.

This produced a nearly-sent **false S1 against SDK**. `test_engine_child_env.py` reported F015
unfixed and had `/proc/<pid>/environ` output showing `WHEEL_ENGINE_SECRET` and `WHEEL_VAULT_KEY`
in a live agent child. The fix was already on `main` and working; the image had been rebuilt
under me between my build and my assertions. The evidence was real and the conclusion was wrong,
which is the worst combination a bug report can have.

**Fixed:** `pin_image()` in `qa/integration/wheel_client.py` resolves the tag to its immutable
image ID once at startup; every container in the run comes from the ID, and the report prints it,
so a result names the build it describes.

**Worth copying:** anyone testing against a shared tag on this host has the same hazard.

## 018 — A skipped positive control silently un-guarded its assertions · S2 · QA (mine) · CLOSED

`SEC-child-env/sentinel-works` proves the digest search can find a secret that IS present, so
that "no secret found" means something. It skipped (my vault key was 31 bytes, so nothing could
be stored) — and `SEC-child-env-no-secret-under-any-name` then reported **green**, against a
child that at that moment held both engine secrets.

The absence assertions did not know their control had not run. A control that does not gate its
dependents is decoration.

**Fixed:** the control returns a boolean; its dependents report `skip` with the reason when it is
unproven, never `pass`. Same class as the vault at-rest scan reading `wheel.db` while the engine
writes WAL: the canary was "absent" because it was in `wheel.db-wal`, and only the control
(the key NAME was missing from the same scan) caught it.
