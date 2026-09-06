# Open bugs

Filed by QA (and ADVERSARY) against `docs/TESTPLAN.md` IDs. Severity: **S1** data loss / security /
privilege escalation · **S2** spec violation · **S3** wrong but workaroundable · **S4** polish.

A bug is closed only when its TESTPLAN ID goes green — not when someone says it's fixed.

**Numbers 017, 018, 019 and 020 were each issued twice**, by concurrent sessions of me filing
into this file from separate worktrees and merging cleanly — the numbers do not collide in a
diff, only in meaning. I have not renumbered them, because commit messages, test comments and
messages already sent cite them; renumbering would silently repoint those. **Cite the title, not
the number.** The pairs are: 017 = "vault-at-rest grepped an empty file" and "suites can test an
image another agent replaced mid-run"; 018 = "id-traceability ignored any ID containing %" and
"a skipped positive control silently un-guarded its assertions"; 019 = the WITHDRAWN wheeld
build, the WOW-clone turn, and API's S1 wheeld compile break; 020 = "failure artifact written
after the container was destroyed" and "CARGO_HOME is 0755". This is itself a defect in the
system of record and the reason the next entry is 023.

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

## 006 — Per-crate coverage below the §0b bar · S2 · SDK+API · CLOSED 2026-09-06

**Closed on CI run 34016518762 (d1e4381), verified by reading the run rather than the report.**
Every crate is over the 90% bar:

| crate | lines | |
|---|---|---|
| `wheel-api` | 90.62% (2000/2207) | ok |
| `wheel-cli` | 94.39% (555/588) | ok |
| `wheel-core` | 97.40% (1647/1691) | ok |
| `wheel-host` | 90.43% (1710/1891) | ok — from 82.11% |
| `wheeld` | 91.73% (344/375) | ok — from 79.20%, a new crate that met the bar without ever being exempted |
| `wheel-engine` | ratcheted (PM ruling), floor rises with each merge, hard expiry at M2 |

API reported the number and explicitly did NOT declare the bug closed, on the grounds that
the ID goes green before the bug does and the close is mine. That is the right order and it
is worth recording that they held it.

**The history matters more than the close.** Most of the original table was wrong: coverage
was summed across six worktrees of each crate, so `wheel-core` read 69.56% while it was
actually 96.96%, and `wheel-cli` read 0.00% while it was 86.88%. SDK carried a failure that
was never theirs. Fixed in `qa/tools/coverage_gate.py` — private `CARGO_TARGET_DIR`,
worktree-scoped file filtering, an empty result is a SKIP rather than 0%, an OOM-killed test
binary is a SKIP rather than a FAIL, and a crate whose DB-gated suites did not run is
INCONCLUSIVE rather than failed.

Still deliberately uncovered in `wheel-host`, on the record rather than as a later surprise:
the privilege drop (setgroups/setgid/setuid/no_new_privs/rlimits), the recursive chown of a
project tree, and the real engine spawn — roughly 60 lines that cannot execute without root
or a real engine binary. API asked PM for a root-capable CI job (`WHEEL_CI_HAS_ROOT=1`) and
did not write tests that route around them to make the number. That restraint is the correct
call and the ask still stands.

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

## 019 — WITHDRAWN. `wheeld` compiled fine; the break was in MY build, not on main · QA (mine)

**I filed this as an S1 against API and it was wrong. Withdrawn, with the mechanism recorded,
because the way I got it wrong is more useful than the bug I thought I had.**

PM produced the contradicting evidence: CI run 34027530976 (head `b15e9c7`, integration job)
ran the wheeld smoke and got `5 passed, 1 failed` — so the binary built and served. That cannot
be true of a workspace that does not compile.

Reconciled from git rather than from argument:

* `bc2ca7a` added `pub ready` to `wheel_host::HostState` **and** `ready:` to
  `crates/wheeld/src/lib.rs` in the SAME commit. No commit on `main` ever had one without the
  other, so the window I claimed never existed.
* At `b15e9c7` neither side had the field. It compiled, which is what CI observed.

**What actually happened to me.** `~/.cargo/config.toml` points all six worktrees at ONE shared
`target-dir` (contract §1, for build throughput). My worktree was on a commit *before* `bc2ca7a`
while another agent's worktree had already built `wheel-host` from *after* it. My
`cargo build -p wheeld` linked against that newer `wheel-host` rlib while compiling my older
`wheeld` source — so the compiler correctly reported a field my source did not set and the
dependency required. Source and dependency from different commits, in one build.

**Why I did not catch it.** I *did* suspect staleness: I saw the failure, noticed my worktree was
behind, rebased, and rebuilt before filing. That felt like ruling staleness out. It ruled out one
kind — a stale *checkout* — and I never considered the other, a stale *artifact*, despite having
fixed exactly that hazard for the coverage gate hours earlier by giving it a private
`CARGO_TARGET_DIR`. I knew the shared target dir corrupts measurements and did not think it could
corrupt a compile.

**Fixed:** `test_wheeld_smoke.py` now builds into its own target dir, so the suite can never again
compile one commit's source against another's artifacts. Anything of mine that shells out to
cargo needs the same treatment.

**The real finding is unaffected and stands with API:** `WHEELD-engine-reachable` fails with
`502 engine_unreachable` — the per-project engine does not answer through the API in the same
process. That is what CI caught, it is genuine, and PM had already routed it. My false S1 cost
API nothing except the noise of it.

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

## 019 — WOW-clone: the agent's turn never reports back · OPEN · QA investigating

`WOW-clone` sends the clone command as a normal user message (`<<FAKE:SH_B64=…>>`), which
travels the real delivery path, and polls `GET /v1/agents/:id/log` for the `exit=` line the
fake writes when the command finishes. Green: `WOW/setup`, `WOW-vault-token`,
`WOW-no-token-in-log`. Red: `WOW-clone` — 180 s with no `exit=`, and the log holds only the
echoed `<AgentPrompt>`, i.e. the message was delivered and nothing came back.

**Not yet attributed, deliberately.** Three candidates and I have evidence for none:
the child never ran the turn; the turn ran and its result never reached the log; or the clone
itself hung on the network inside the sandbox. `SEC-vault-env-scope/wired` already proves the
vault value reaches the child's environment, so a missing `GH_TOKEN` is ruled out.

Next step is the engine log for a failed run, which is why 020 mattered.

## 020 — The failure artifact was written after the container was destroyed · S3 · QA (mine) · CLOSED

`run_suite()` saves `docker logs` to `qa/artifacts/<suite>-engine.log` before cleanup, on
failure only. But `main()` had its own `finally: docker rm -f`, which ran first — so the
artifact for the one run anybody would ever read it for contained 70 bytes of
`Error response from daemon: No such container`.

Two owners of teardown, and the one that ran first was the one that did not know about the
evidence. `main()` no longer tears down; `run_suite()` owns it, capture then remove.

## 019 — `wheeld` does not compile on main: the one binary we ship · **S1** · API

**2026-09-06 REOPENED — the fix is partial; it still does not compile.** `wheeld` now SETS
`ready`, but with the wrong type:

```
expected `Readiness`, found `Arc<Atomic<bool>>`
   Arc::new(std::sync::atomic::AtomicBool::new(true)),
error: could not compile `wheeld` (lib) due to 1 previous error
```

Same root cause as the original, one step along: `HostState` is constructed field-by-field at
a second call site, so each change to the struct is a separate chance to get it wrong. This is
the second compile break from the same seam in one day, which is the argument for the
constructor rather than a second manual fix.

Verified on origin/main after fetch+rebase, not on a stale tree.

`cargo build -p wheeld` on `origin/main` (verified at b15e9c7, after rebasing — my first
observation was on a worktree that was behind, so I withheld the report until I had re-run it
on current main):

```
error[E0063]: missing field `ready` in initializer of `HostState`
   --> crates/wheeld/src/lib.rs:121:8
121 |     Ok(wheel_host::HostState {
    |        ^^^^^^^^^^^^^^^^^^^^^ missing `ready`
error: could not compile `wheeld` (lib) due to 1 previous error
```

`wheel_host::HostState` gained a `ready` field (`d8da69d`, "host: cover the parts that were
only ever exercised in production") and `wheeld`, which constructs one, was not updated.

**Why S1 rather than S2.** `wheeld` is M1.7's entire promise: one executable, nothing
installed. A workspace that does not build means the artifact a new user downloads does not
exist. It is also `members = ["crates/*"]`, so this breaks `cargo test --workspace` for
everybody, not just whoever runs the daemon.

**How QA saw it and CI did not (yet):** the wheeld smoke tried to build the binary it smokes
and skipped by name with the compiler error, which is the behaviour I want — it did not pass,
and it did not pretend the daemon was fine. The CI run I diagnosed from (34020280822) predates
the break; run 34027530976 on b15e9c7 is the first that should catch it.

**Fix:** set `ready` in `crates/wheeld/src/lib.rs:121`, or give `HostState` a constructor so a
new field cannot break a second call site silently — the second is the reason this happened.

## 020 — `CARGO_HOME` is 0755: every uid in the sandbox can read a tenant's fetched sources · S2 · SDK

`crates/wheel-engine/src/supervisor/mod.rs:390` does `data_dir.join("cargo")` +
`create_dir_all`, which takes the default mode. Observed on a live engine:

```
CARGO_HOME='/data/cargo' mode='755'
```

ADVERSARY 029 (PM ruling 2026-09-06) requires it private to the project uid, 0700.

**Why it matters here rather than in general.** §2 gives every agent, script and MCP child
inside a sandbox its **own uid**. So "readable by other uids" is not hypothetical — it is the
design. What a tenant *fetches* is the thing that must not be shared: downloaded sources, and
any registry credentials a project configures. `RUSTUP_HOME` is the opposite case and is
allowed to be shared precisely because it is immutable and read-only.

**Half of my first report was wrong and is not being filed.** My assertion also demanded the
literal path `/data/projects/<id>/.cargo` from 029. That is the *process* backend's layout;
under docker there is one engine per project with its own volume, so the engine's data dir is
already project-scoped and `/data/cargo` satisfies "per project". Asserting the string would
have failed a correct docker deployment. The check now tests the property, not the path.

**Why it surfaced only now:** `WOW-toolchain-cargo-per-project` had been skipping with "the
fake harness did not record CARGO_HOME" — the harness recorded `CLAUDE_CONFIG_DIR` and
`CODEX_HOME` but not the toolchain vars, so the criterion read as covered in the plan and
executed nothing. Fixed in the same change (754f138).

**Acceptance test:** `WOW-toolchain-cargo-per-project`. It is red now and goes green on the fix.

> Filed in the repo rather than sent: this session's yoke token was refused mid-report and
> cannot be refreshed in-process. The documented override runs as the admin key, ROOT and
> unattributed, which would put my findings under someone else's name — so it is not used.
> Per the contract, `qa/BUGS.md` in git is the system of record and a message is only a
> notification; this is the record.

## 021 — CARGO_HOME is one shared dir at 0755, not per project · S2 · SDK

Found by `WOW-toolchain-cargo-per-project`, the acceptance test PM assigned for ADVERSARY 029.
The RUSTUP_HOME half passes; this half does not.

```
CARGO_HOME='/data/cargo' mode='755'
```

`supervisor/mod.rs:390` does `self.cfg.data_dir.join("cargo")` — one directory for the whole
project data dir, created with default permissions.

**Two ways this misses 029.** It is not per project: on the process backend `/data` is the
host's, so `/data/cargo` is shared across every project on the machine, which is exactly the
cross-tenant leak the comment above that line describes wanting to prevent. And at 0755 it is
world-readable inside the sandbox, so even within one project every other uid can read it —
§2 gives each agent, script and MCP child its own uid precisely so they are not each other's.

**What is in there.** Downloaded sources and, if a tenant ever configures one,
`~/.cargo/credentials.toml` with a registry token. The code comment already names this risk;
the implementation just lands one level too high.

**Fix per 029:** `/data/projects/<id>/.cargo`, mode 0700, owned by the project uid.

**Credit where due:** the comment beside the bug is right about why it matters. This is the
implementation not matching its own stated intent, which is the kind that survives review
because the reasoning next to it reads correctly.

## 022 — The engine's journal check reads the mode back instead of proving it · **S1** · SDK

`ENG-starts-without-shm` is RED against an image built from current main, i.e. **after** the fix
that recovered production. The engine dies on a shm-less volume with:

```
wheel-engine: applying schema: attempt to write a readonly database: Error code 8
```

**Why production recovered anyway:** `e1ce934` puts `locking_mode=EXCLUSIVE` in
`wheel-host/src/store.rs`, which keeps the WAL index in heap and never opens a `-shm`. The host
is what opens first on the deployed machine, so the crash loop stopped. The ENGINE's own
database path did not get the same treatment.

**The defect, in `db/mod.rs::set_journal_mode`:**

```rust
let _ = conn.pragma_update(None, "journal_mode", wanted);
let mode = current_journal_mode(conn)?;
if mode.eq_ignore_ascii_case(wanted) { return Ok(mode); }   // <- fast path
drain_under_exclusive_lock(conn, wanted)?;                  // <- never reached
```

On a filesystem that cannot map a `-shm`, `PRAGMA journal_mode=WAL` **reports `wal`**. Measured:

```
journal_mode after WAL attempt: wal
first write:                    attempt to write a readonly database
```

So the fast path matches, returns `Ok`, and the drain — the whole recovery mechanism, and the
call site SDK already knew was untested — is never reached. The failure surfaces later at
`migrate()`, as a schema error, which is why it reads as a corrupt database rather than an
unusable journal mode.

**The read-back is the bug.** The function's own comment says "the pragma's own result is not
evidence" and then treats a *read-back* as evidence, which sqlite is equally willing to lie
about here. PM's earlier version proved WAL with `BEGIN IMMEDIATE; COMMIT;` — a write. That
proof is not in this path.

**Fix:** prove the mode with a write before returning on the fast path, not just after the
drain. `drain_under_exclusive_lock` already ends with `BEGIN IMMEDIATE; COMMIT;` for exactly
this reason.

Found by the gate PM asked for, on the first run after the production fix. The host is safe;
any project engine on such a volume is not.

**Two things I checked before leaving this filed, because both would have made it wrong:**

*Is it a permissions artifact of my fixture?* No. Re-run with the `-shm` directory owned
`agent:agent` — writable by the very uid the engine runs as — and it still exits 1 with the same
error. The block is the directory being a directory, not a mode.

*Did the wheel-sqlite refactor undo the host's fix too?* I thought so from reading:
`open_configured` tries `configure_journal_to` first and only falls through to the EXCLUSIVE
escape on `Err`, and I had measured WAL reading back as `wal`. Ran it instead — host role, blocked
volume — and it starts: *"reconcile complete; project routes are open"*. A fresh `host.db` stays
in `delete`, the read-back mismatches, and the fall-through works. **The host is fine and I would
have filed a false S1 on a sound-looking inference.**

That asymmetry IS the bug, stated precisely: the host calls `open_configured(path, true)` and has
a second line of defence; the engine calls it with `false` — it cannot take the file exclusively
because `tables::query` opens it a second time — so for the engine the read-back is the *entire*
protection, and on a database that does report `wal` there is none.


## 023 — `free_port(0)` handed back port 0, and four gates failed against a healthy engine · S2 · QA (mine) · CLOSED

**TESTPLAN:** `ENG-journal-override-cannot-disable-recovery/*`

`free_port(preferred)` returns `preferred` if it is bindable and any free port otherwise.
Binding to port **0 succeeds** — the kernel assigns an ephemeral port — so `free_port(0)` reported
0 as bindable and handed back `0`. The override loop then published the container on a random
port and probed `http://127.0.0.1:0/healthz`, which cannot reach anything, for 20 seconds per
case.

All four override cases failed identically. The report I had drafted said the override still
disables the recovery path on `main` — during an outage caused by exactly that, to the operator
who caused it. It was wrong; the engine was healthy in all four.

**What stopped it** was not discipline, it was the shape of the evidence: four identical
failures, and inside my own failure text the engine's log line `"message":"database ready"`. A
healthy engine does not say that while failing to start. The failure detail I had written for
someone else's benefit is what caught it.

**Fix:** `free_port(0)` now means "any free port" and returns the kernel-assigned one, so every
suite is protected rather than this one call site. The incident is in the docstring, because the
next person to read it will be reading it for a reason.


## 024 — Both panic gates passed against the engine that took the board down · **S1** · QA (mine) · CLOSED

**TESTPLAN:** `ENG-escaper-never-panics`, `ENG-escaper-engine-survives`

PM asked for two gates during a 56-minute outage. I wrote them, they went green, and they were
worthless. Verified by building `wheel-engine` from `455b753^` — the exact pre-fix source that
was live during the outage — and running them against it: **all green.**

Two independent reasons, both mine:

1. **The agent was never started.** `escape_envelope_body` has one production call site,
   `Message::envelope` at `supervisor/mod.rs:545`, and it runs at **delivery**. A message to a
   stopped agent queues; the escaper is never called. The gate proved that an engine stays up
   while a code path does not execute.
2. **The inputs could not panic.** The shape is `<` with a multi-byte character straddling
   `name_at + TAG.len()`. Mine put the character before the `<` or after a complete
   `</AgentPrompt>`, where every offset is a character boundary. Measured killing shape: `<` or
   `</`, then 9–10 ASCII bytes, then a 3-byte character (8–10 for a 4-byte one).

**Fix:** the agent is started, the fake harness writes a transcript, and a delivered benign body
found in that transcript is the control (`ENG-panic/escaper-runs`) — so "the engine stayed up"
can never again mean "the code never ran". Offsets 0..14 are walked for em dash, emoji, CJK and a
ZWJ family. Now RED on `455b753^`, GREEN on `main`.

**What the mutation also found**, which no amount of reading would have: at the moment of the
panic the process does **not** die. Send drops the connection, `/healthz` answers **200**, the
container is **running**, and a benign message sent afterwards is **never delivered**. The tokio
task unwinds alone and the delivery loop is gone while every healthcheck passes. Filed to SDK as
a policy question and gated as `ENG-delivery-survives-escaper`, which asserts delivery rather
than liveness.

**The rule this cost me:** a gate written during an incident is written under the worst
conditions I will ever have, and mine was wrong twice over in ways that both read as success. A
new gate is not finished when it is green. It is finished when I have watched it go red against
the defect it names.


## 025 — `make check` reported `main` red on a run that never happened · S2 · QA (mine) · CLOSED

**TESTPLAN:** the merge gate itself

`make check` on `main` came back `✗ rust:test FAILED (1108s, exit 101)`. The whole of the
evidence was:

    Running tests/boot_db.rs (/Users/metatron/wheel-target/debug/deps/boot_db-573d978886d7ee4f)
    error: test failed, to rerun pass `-p wheel-api --test boot_db`

No test list, no assertion, no panic, no compiler error. Re-run alone it passes 5/5 — from a
binary with a **different hash** (`boot_db-c148eb42…`), after a full 19-minute recompile. Six
worktrees share one `target-dir` by contract; `qa/check.sh` serialises its own gates through
`qa/tools/with_lock.py`, but nothing stops a dev typing `cargo test` in their own worktree, and
that rewrites artifacts underneath a run in progress.

`main` was not red. I nearly told the team it was, which is an instruction to five agents to stop
what they are doing.

**Fix:** `rust:test` now runs through `qa/tools/cargo_test_gate.py`. A cargo failure counts as
FAILED only when the output carries evidence of one — a failing test, a panic, a compiler error,
or a killing signal. Otherwise it exits 75, which `check.sh` already renders as "did not run",
and the whole run is INCONCLUSIVE rather than red. Verified both directions: an evidence-free
exit 101 becomes 75; `test result: FAILED`, `error[E0308]` and signals stay 101.

**Second time this mechanism has bitten.** The first was BUG-019, where I filed an S1 against
another agent's crate and PM produced CI evidence contradicting it. The lesson I wrote then was
about my build; the lesson that was actually needed is this one — the gate has to distinguish
"broken" from "could not tell", in that direction too. A gate that cries wolf gets ignored
exactly when it is right.


## 026 — "Position is an integer cell" is ruled but not implemented · S2 · SDK · OPEN

**TESTPLAN:** `POS-is-an-integer/*`, `POS-rounds-and-clamps/*`, `POS-move-clamps` — 17 red on
`main`.

`docs/ARCHITECTURE.md:61` ("Position is an integer cell", operator ruling 2026-09-06) says the
engine rounds on the way in, clamps out-of-range values to ±32767 rather than rejecting them, and
**returns the clamped value it stored**. `crates/wheel-core/src/node.rs:97` is still:

    pub struct Position { pub x: f64, pub y: f64 }

with no rounding and no clamping anywhere in the crate. Measured against the engine: `10.4`
comes back `10.4`, `99999` comes back `99999.0`, and a PATCH past the bound returns
`{'x': 99999.0, 'y': -99999.0}`.

**Why S2 and not S4.** The ruling's own reasoning is the failure mode: rounding and clamping are
implemented twice, once each side, and the operator-visible bug is not wrong arithmetic but the
two halves drifting — a node that appears to save and springs back to a different place on the
next refetch. While the engine returns whatever it was handed, the client's clamp is the only
one, so the two views disagree by construction for any value outside the bounds.

The gate deliberately asserts **agreement** (write response == later board refetch) rather than
the arithmetic, so it will go green on any implementation that is internally consistent.
