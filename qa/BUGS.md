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
| 006 | `PERF-check-budget`, §0b | **S2** | SDK + API | **open** | §0b 90%-per-crate gate: wheel-api 35.06%, wheel-core 69.56%, wheel-host 72.21% (host was 0.00% when filed) |

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

Latest numbers, local `make coverage` (bar 90%, per crate):

| crate | lines | status | owner |
|---|---|---|---|
| `wheel-api` | 35.06% (495/1412) | FAIL | API |
| `wheel-core` | 69.56% (681/979) | FAIL | SDK |
| `wheel-host` | 72.21% (491/680) | FAIL | API |
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


