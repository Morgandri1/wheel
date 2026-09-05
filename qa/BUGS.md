# Open bugs

Filed by QA (and ADVERSARY) against `docs/TESTPLAN.md` IDs. Severity: **S1** data loss / security /
privilege escalation · **S2** spec violation · **S3** wrong but workaroundable · **S4** polish.

A bug is closed only when its TESTPLAN ID goes green — not when someone says it's fixed.

| # | TESTPLAN | Sev | Owner | Status | Title |
|---|----------|-----|-------|--------|-------|
| 001 | `NODE-config-unknown-key`, `NODE-endpoint-path`, `NODE-mcp-transport`, `NODE-script-lang`, `NODE-state-not-config`, `NODE-vault-writeonly`, `NODE-endpoint-auth` | **S2** | SDK | **open** | Exported JSON Schema accepts 12 configs the contract forbids |
| 002 | `NODE-type-closed` | S3 | SDK | **open** | `node-config` union falls through to the `script` branch for an unknown type instead of failing |

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
