# Open bugs

Filed by QA (and ADVERSARY) against `docs/TESTPLAN.md` IDs. Severity: **S1** data loss / security /
privilege escalation · **S2** spec violation · **S3** wrong but workaroundable · **S4** polish.

A bug is closed only when its TESTPLAN ID goes green — not when someone says it's fixed.

| # | TESTPLAN | Sev | Owner | Status | Title |
|---|----------|-----|-------|--------|-------|
| 001 | `NODE-config-unknown-key`, `NODE-endpoint-path`, `NODE-mcp-transport`, `NODE-script-lang`, `NODE-state-not-config`, `NODE-vault-writeonly`, `NODE-endpoint-auth` | **S2** | SDK | **open** | Exported JSON Schema accepts 12 configs the contract forbids |
| 002 | `NODE-type-closed` | S3 | SDK | **open** | `node-config` union falls through to the `script` branch for an unknown type instead of failing |
| 003 | `NODE-tool-config` | **S2** | SDK | **open** | `ToolConfig` diverges from §3d: no `kind`, and `source{format,raw,imported_at}` flattened to `source_format` |
| 004 | `WM-export-conformance`, `WM-endpoint-vault-read`, `WM-script-tool-read` | S3 | SDK | **open** | `wire_allowed` is missing TWO contract rows: `endpoint→vault (read)` and `script→tool (read)` |

---

## 001 — Exported JSON Schema accepts 12 documented-invalid node configs · S2 · SDK

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
