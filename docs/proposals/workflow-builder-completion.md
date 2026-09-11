# Workflow Builder — finishing it end to end (for PM ruling)

Builder (Wheel), 2026-09-11. Scope: `docs/proposals/workflow-builder-feature.md`. Prompt:
`docs/BUILDER_PROMPT.md`. Builds on `apply-step-constraints.md`, `board-apply-shape.md` and
`board-apply-client-contract.md`. Task 5 of `docs/wow-agent-brief.md` is referenced only for its
relationship to this work (§3). **Decisions needing a ruling are marked RULING.**

## 0. Audit: what existed, and what the audit found broken

Read from the code on `origin/main` 94dea07, not taken from the brief.

| Piece | State on main |
|---|---|
| `POST /v1/projects/{id}/board/apply` (`wheel-api/src/apply.rs`, `routes/board_apply.rs`) | Built. Validate-then-apply, dry run, `allow_patch`/`allow_wire`, typed refusals, 207 partials. Plan kinds: `create_nodes`, `patch_nodes`, `create_wires`. No delete, no unwire. |
| `POST /v1/projects/instantiate` | Built, reuses `validate`/`execute`. |
| `BuilderPanel`, `workflow-proposal.ts`, `board-apply.ts` | Built, but not mounted anywhere. `runner` is `null` everywhere. |
| Conversation backend | **Missing.** Nothing runs the builder, and no code references `BUILDER_PROMPT.md`. |
| `WHEEL_HARNESS_AUTH` | Spawn gate plus periodic re-check are live (`supervisor/mod.rs`). The route-level points (auth/begin, auth/complete, vault PUT) are **not** implemented. |

Five defects in the existing apply path. Each one would have shipped the moment the builder was
mounted:

1. **Every builder wire is silently dropped.** The prompt's contract puts wires *on the node*:
   `{"id", "wires":[{"to": <id>, "type"}]}`. `EmittedBoard` reads only a top-level, name-keyed
   `wires` list, and serde ignores unknown keys. So a builder board applies as nodes only, with
   zero wires, and reports `applied: true`. That is the "refusal is never a drop" invariant,
   broken on the builder's own output format. (Templates use the flat form, which is why nothing
   caught it.)
2. **Patching an existing node clobbers fields the board never mentioned.** `execute` sends
   `serde_json::to_value(&typed_config)`. Typed deserialisation turns an *omitted*
   `run_on_startup` or `ephemeral_context` into an *explicit* `false`, and the RFC 7386 merge
   then writes that `false` over the stored `true`. This is exactly the "does not clobber
   untouched config" acceptance criterion.
3. **A codex agent passes pre-validation, then fails live.** The engine refuses
   `harness: codex` at `POST /v1/nodes` (`board_routes::reject_unsupported_harness`). Apply does
   not, so a codex board gets a clean preview and then a 207.
4. **An unchanged node re-emitted by "improve" is planned as a patch** and needs `allow_patch`,
   although nothing would change.
5. **A malformed board gets axum's plain-text 422.** It has no `refusals`, so the confirm step
   can only say "refused" with no reason.

Capability truth, verified against the engine rather than the prompt:
- **codex**: refused at creation. That is a refusal, not a "not runnable" note.
- **script**: creatable; nothing executes `ScriptConfig`.
- **chest**: creatable; storage unimplemented (`cli_routes`: "listing a chest node is not
  implemented yet").
- **mcp**: creatable, but **nothing attaches an MCP node to an agent**. The only MCP config
  written is the engine's own `wheel mcp-serve`.
- Live: claude agent, ctx, table, endpoint, vault, tool.

`BUILDER_PROMPT.md`'s capability section claims mcp attaches its tools; it is corrected in this
change.

## 1. The conversation backend

### Decision: a transient, engine-managed run — `POST /v1/builder/turns` on the project's engine

- **Engine** (engine-secret realm): `POST /v1/builder/turns`. It answers
  `text/event-stream`, and is where the run happens.
- **API** (owner-scoped): `POST /v1/projects/{id}/builder/turns` is a dedicated streaming
  passthrough.
  - It is dedicated, not the generic `/engine/{*rest}` proxy, for one measured reason: the API's
    shared `reqwest` client has a 30s whole-request timeout (`boot.rs`), which would cut a
    builder turn mid-stream. This route sets its own per-request ceiling.
  - Ownership is proven by `ProjectScope` before a byte is forwarded, as for every
    project-scoped route.
- **Credential management** (`GET`/`PUT`/`DELETE /v1/builder/credential`) goes through the
  existing generic engine proxy, like `agents/{id}/auth/*` does today.
- **AgentGrid** (Phase 1b, local sidecar) calls the engine route directly with its bearer; a
  hosted client calls the API route. The frames are identical, so there is one client.

Why the engine and not the API or the web:
- **Credentials.** The user's claude credentials live in the engine: per-node stores and vaults.
  Running the builder anywhere else means moving a credential out of the engine. Running it in
  the engine means nothing crosses a boundary it doesn't already cross.
- **The new-project case.** A project has an engine the moment it has a container, even with an
  empty board. So the builder works without being a node on the board it is designing.
- **No shared or privileged run.** The engine spawns under the same `child_command` discipline as
  every agent (`env_clear` plus the platform allowlist). There is no server-held Anthropic key
  anywhere in the path.

### Request

```jsonc
POST /v1/builder/turns            // API: /v1/projects/{id}/builder/turns
{
  "mode": "new" | "improve",       // improve = the ENGINE reads and attaches the current board
  "turns": [ {"role": "user"|"builder", "text": "…"} ],   // last turn must be the user's
  "credential": {"source": "builder"}                     // default
              | {"source": "agent", "node": "<uuid>"}     // use that claude agent's credential
              | {"source": "vault", "node": "<uuid>"}     // use that vault's claude key
}
```

In improve mode the board comes from the **engine's own `GET /v1/board`**, never from the client.
A client-supplied "current board" would let a caller make the builder reason about a board that is
not the one the apply will validate against.

### Response: SSE frames

```
event: delta   data: {"text": "…"}                        // streamed text (partial messages)
event: done    data: {"text": "<full reply>", "boards": 1} // authoritative full text; boards = marker count
event: error   data: {"code": "needs_auth"|"builder_error"|"timeout"|"too_long", "message": "…"}
```

Refusals that happen before anything spawns are ordinary JSON errors with a status:

| status | code | meaning |
|---|---|---|
| 409 | `needs_auth` | No credential resolved. Body adds `sources: {agents:[{id,name}], vaults:[{id,name}]}`, which lists what the user could designate instead. |
| 403 | `policy` | `WHEEL_HARNESS_AUTH=api-key-only` and the resolved credential is OAuth-shaped. |
| 429 | `builder_busy` | This project already has a builder run in flight. |
| 400 | `invalid` | Bad mode, empty or oversized conversation, last turn not the user's, unknown or wrong-type node. |
| 413 | `invalid` | The current board is too large to hand to the builder. |

### The run

- **Program and flags.** The engine runs `claude` via `child_command` with:

  ```
  --print --input-format text --output-format stream-json --verbose --include-partial-messages
  --system-prompt-file <run>/system.md --tools "" --strict-mcp-config
  --no-session-persistence --max-turns 1
  ```

- **System prompt.** `BUILDER_PROMPT.md` is compiled into the engine with `include_str!`, so the
  doc *is* the prompt: one copy, no drift. It is written to a 0600 file in a per-run 0700 scratch
  dir and passed **by path**. The conversation and the board go to **stdin**. No prompt content
  ever appears on a command line (argv is world-readable across uids; the same rule the agent
  driver follows). A test runs the real fake harness and asserts that argv carries no prompt or
  conversation text.
- **No tools, no board.** The run gets:
  - no MCP config, plus `--strict-mcp-config`, so no user or project MCP sneaks in;
  - no `WHEEL_TOKEN_FILE` and no `WHEEL_ENGINE_URL`;
  - no built-in tools (`--tools ""`);
  - the default permission mode, **not** `bypassPermissions`;
  - `cwd`, `HOME` and `CLAUDE_CONFIG_DIR` all set to the empty scratch dir, which is deleted
    afterwards.

  Its only output is text, and its only effect is a proposal a human must confirm.
- **Framing.** Turns are wrapped as `<turn role="…">…</turn>` inside `<conversation>`, and the
  board goes in `<current_board>`. Closing tags inside content are escaped so a turn cannot close
  its own frame. This separates "what the user said" from "data about the board". It is framing,
  not a security boundary; the boundary is §4's apply gate.
- **Board redaction.** Before the board goes to the model:
  - agent `state` (sessions, errors, spend) is dropped;
  - every MCP `env` **value** is replaced by `"<redacted>"`, since users type tokens there;
  - an imported tool's `source.raw` is replaced by its size.

  Vault values are never on the board to begin with.
- **Limits.**
  - At most 40 turns, 16 KiB per turn and 128 KiB in total. The board is capped at 256 KiB
    after redaction.
  - Output is capped at 512 KiB; past that the child is killed and the client gets `too_long`.
  - Wall clock is 240s, then the child is killed and the client gets `timeout`.
  - **One run per project at a time.** Every turn spends the user's money, and a runaway client
    must not be able to fan out.
- **Streaming.** `--include-partial-messages` text deltas become `delta` frames. If the CLI sends
  none, the whole assistant message becomes one `delta`. `result` becomes `done`, carrying the
  **result text**, which the client treats as authoritative over its accumulated deltas.
- **Output contract.** `done.boards` counts `---START-WORKFLOW---` markers. The UI already
  parses the *last* block, and when `boards > 1` it tells the user the builder emitted more than
  one board. The prompt's "exactly one" becomes observable rather than assumed.

### Credential source (RULING requested on the default order)

Resolution, first match wins:

1. **An explicit `credential` in the request.**
   - `agent` must be a claude agent node. Its credential resolves the way that agent's own spawn
     would: a wired vault's claude key first (vault wins, as at spawn), then its `wheel-token`,
     then its native `claude auth login` store (read with `oauth_token_from_store`, the same
     function `save_to_vault` uses).
   - `vault` must be a vault node holding `ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN`.
     Holding both is refused as ambiguous, the same rule agents follow.
2. **Otherwise, the builder's own store**: `creds/builder/wheel-token`, 0600 in a 0700 dir.
   - Set with `PUT /v1/builder/credential {api_key | setup_token}`, using the same
     `store_token`/`classify_token` as `auth/complete`.
   - This is what makes entry point 1 work: a brand-new project has no agents and no vaults to
     designate.
3. **Nothing resolved**: `409 needs_auth`, listing the agents and vaults the user could
   designate. The UI offers those, or a paste field for the builder's own store. That paste field
   is the existing `api_key`/`setup_token` flow, pointed at the builder's store.

**Exactly one credential variable is ever exported** (`token_env(classify_token(..))`), which is
the same rule as agents. The engine's own environment never contributes one.

`WHEEL_HARNESS_AUTH` is enforced in two places:
- **At the store:** under `api-key-only`, `PUT /v1/builder/credential` refuses a `setup_token` or
  any OAuth-shaped value (403 `policy`).
- **At spawn, which is the gate that holds:** the resolved credential is classified immediately
  before spawning, and an OAuth-shaped one is refused with 403 `policy`. That covers an agent's
  native login store or a vault value that never passed through a Wheel route.

There is no periodic re-check, because a builder run lives at most 240s and never outlives the
request that started it.

Not built: **paste-code OAuth for the builder's own store.** `claude setup-token` covers the
subscription case with a durable token, and designating an already signed-in agent covers the
rest. Adding the login child for a store that is not a node would duplicate `oauth.rs` for little
gain.

## 2. "Improve existing" and the diff

### Seeding

Improve mode attaches the engine's own current board (§1). The builder prompt gains an explicit
improve contract (see "RULING: explicit removals"):
- Re-emit only the nodes you add or change.
- An existing node keeps its `id` **and** its `name`.
- A wire may target an existing node's id.
- Removals go in a top-level `"remove": {"nodes": [name…], "wires": [{from,to,type}…]}`.

### RULING: removals are explicit; omission never deletes

Two ways to express a delete:
- **Omission:** the emitted board is the complete desired state, and whatever it leaves out gets
  deleted.
- **Explicit:** the board names each removal.

I recommend **explicit**, for adversarial reasons:
- **Truncation.** LLMs abbreviate ("…the other nodes are unchanged"). Under omission semantics a
  truncated or lazy reply *is* a mass-deletion proposal. Under explicit semantics it is a no-op.
- **Injection by omission.** Improve mode hands the builder board content that board agents can
  write, such as ctx markdown behind an agent→ctx write wire, and third-party OpenAPI text on
  tool nodes. Under omission semantics, content that persuades the builder to *leave a node out*
  proposes deleting it, and it reads as an innocent "here's the updated board". Under explicit
  semantics every deletion is named in the reply the user reads, and again in the plan.
- **The honest cost:** a builder that forgets to list a wire removal leaves the wire in place. The
  plan shows "remove 0 wires", which the user can see before applying.

I judged "keeps a capability the user wanted gone, visibly" the lesser failure than "destroys data
because a reply was truncated".

### New plan kinds, each behind its own consent

| plan kind | consent flag | refusal without it |
|---|---|---|
| `delete_nodes: [{name, type}]` | `allow_delete` | `delete_not_permitted` |
| `delete_wires: [{from,to,type}]` | `allow_unwire` | `unwire_not_permitted` |

These are separate from each other and from `allow_patch`/`allow_wire`, because the risks differ:
- **Unwiring** is reversible and reduces capability.
- **Deleting a node** is destructive. A **table** drops its rows (`board::delete` drops
  `t_<name>`), a **vault** destroys its secrets, a **chest** loses its blobs once storage exists,
  and an **agent** loses its messages and logs (schema cascade).

The confirm step lists deletions in their own block, with a per-type data-loss line. A deleted
node's own wires cascade, and the plan shows them under that node rather than as separate unwires.
The 422 `consent` object gains `would_delete` and `would_unwire`, and `grant` names the flags.

Validation, all before anything happens, **every refusal returned**:
- a removal naming a node or wire that does not exist;
- a node both re-emitted and removed;
- an emitted wire touching a removed node;
- a `remove` on a fresh-project apply or template instantiate, which has nothing to remove.

Removals count toward the per-apply caps.

### Read before write (the RFC 7386 finding)

- The apply reads each existing node's **current config** and diffs the builder's **raw emitted
  config** against it. Raw means as written, not re-serialised from typed defaults, which is the
  fix for audit defect 2.
- The patch sent is the minimal merge patch: only the top-level keys whose merged value differs.
  An unchanged node drops out of the plan and needs no consent (defect 4). The **merged** result
  is validated before anything is created.
- Arrays replace wholesale. So a patch touching an array field (today `workspaces`, and any future
  array) is flagged in `patch_details: [{name, fields, replaced_arrays}]`, and the UI says "the
  whole `workspaces` list is replaced (2 → 1 entries)". The user sees the replacement; it is
  never a silent loss.
- **Renames are refused** (`rename_not_supported`) when an emitted node keeps an existing id with
  a new name. A rename would otherwise read as create + orphan. Renaming stays a canvas action.

### Consent is bound to the plan (TOCTOU)

- A dry run returns `plan_digest`: a sha256 over the plan, including the patch bodies.
- An apply may send `expect_plan`. If the plan it recomputes against the board *as it is now*
  differs, it returns **409 `plan_changed`** with the new plan, and nothing is applied.
- **An apply containing deletions must send it.** Without it the answer is 409
  `plan_confirmation_required`. The user confirmed specific deletions, and a board that changed
  underneath must not turn that consent into a different set.

The web client always sends the digest.

### Execution order and partial reporting

The order is:
1. delete wires;
2. create nodes;
3. patch nodes;
4. create wires;
5. delete nodes, **only if steps 1–4 all succeeded**.

The reasoning:
- **Capability is removed before capability is added.** A failure mid-way leaves the board with
  less reach, not more.
- **Destruction goes last and is conditional.** If anything earlier failed, each planned deletion
  is reported as a failure ("not deleted: an earlier step failed, so nothing destructive was
  attempted"). A half-applied improve then never also destroys what it was replacing.

The report gains `deleted_nodes` and `deleted_wires`. `applied` stays true only when every step
landed. The report never says success for a partial, and the status stays 207.

## 3. Relationship to wow task 5 (design only; out of scope)

The builder is **out-of-band and inert**: a transient run with no tools, whose only output is a
proposal a human confirms through `board/apply`. Task 5's helper is **in-band and acting**: a
node with tools and global read/write.

They should share three things:
1. **One gate.** The helper's place, grant and remove tools should drive the *same* validate →
   plan → consent → apply path. They should submit a board delta and get a plan back, not call
   `POST /v1/nodes` directly. That puts the consent flags, the digest, destructive-last ordering
   and the codex/matrix pre-validation under the helper too, for free.
2. **One prompt discipline.** The operator's "do exactly the literal ask" directive for the helper
   is the same discipline §2 encodes structurally ("omission never deletes; removals are named").
3. **One credential rule.** The helper runs on a user credential through normal spawn, with no
   privileged token. The builder's resolution order (§1) is reusable if the helper wants "use the
   builder's credential".

What this change does **not** do for task 5: no global-access wire semantics, no helper node type,
no in-band tools. The builder's conversation route could later serve as the helper's "planner" (it
proposes, the helper executes the confirmed plan). That needs its own proposal and an ADVERSARY
review of the in-band half.

## 4. ADVERSARY threat model

The builder turns LLM output into real nodes, wires and agents. **The LLM is untrusted, and so is
everything it read.**

Assets:
- the user's claude credential (money, and ToS standing);
- board integrity (wires are capabilities);
- data in tables, vaults and chests;
- the engine secret and vault key;
- other tenants.

Adversaries:
- **A1**: hostile or misled builder output, including prompt injection through board content:
  agent-writable ctx markdown, and third-party OpenAPI text on tool nodes.
- **A2**: a board-resident agent. It is untrusted code on the same uid as the engine's children.
- **A3**: another tenant, or an unauthenticated caller.
- **A4**: a buggy or hostile client (browser or AgentGrid).
- **A5**: the project owner trying to use OAuth under `api-key-only`.

| # | Threat | Mitigation | Gate (test) |
|---|---|---|---|
| T1 | Prompt/board text on argv (world-readable) | system prompt by 0600 file path; conversation + board on stdin | engine test runs `qa/harness/fake-claude`, reads its argv dump, asserts no conversation/prompt text and `--system-prompt-file` not `--system-prompt` |
| T2 | Builder gains capabilities | `--tools ""`, `--strict-mcp-config`, no MCP config, no `WHEEL_TOKEN_FILE`/`WHEEL_ENGINE_URL`, default permission mode, scratch cwd/HOME, `--max-turns 1` | test asserts argv lacks `--mcp-config`/`bypassPermissions` and child env lacks `WHEEL_*` |
| T3 | Engine secrets reach the child | `child_command` (`env_clear`), already enforced for every spawn site by `every_child_process_is_started_through_child_command` | existing gate covers the new spawn site |
| T4 | Wrong or privileged credential | only project-stored credentials; exactly one variable; resolution order §1; engine env never contributes | fake-claude `strict_auth` + env digest: the stored key arrives under the right variable and nothing else |
| T5 | `api-key-only` bypass via builder (A5/A2) | refuse OAuth-shaped at `PUT /v1/builder/credential` and at spawn against the *resolved* value | tests: setup_token refused under policy; agent native OAuth store refused at spawn |
| T6 | Secrets in the board reach the model | MCP `env` values redacted, `state` dropped, tool `source.raw` elided | unit test on the redactor |
| T7 | Injection steers a hostile proposal (A1) | apply is the gate. Server re-validates everything; create-only by default; four separate consents; plan shown before apply; removals explicit (omission never deletes); destructive steps last and conditional | apply tests per consent; mutation-checked |
| T8 | Malformed / hostile board | refused **whole**, 422, before anything exists: parse errors (now a JSON refusal), unknown/duplicate ids, renames, codex, invalid names/configs, invalid **merged** configs, illegal wires, unknown removals, conflicting changes, size caps incl. removals | route tests assert 422 + mock engine saw zero writes |
| T9 | Silent drop of builder wires (defect 1) | nested `wires` normalised server-side; an unresolvable `to` is a refusal | test: nested board creates its wires; unknown id → 422 |
| T10 | Clobber of untouched config (defect 2) | raw-config diff; minimal merge patch | test: `run_on_startup: true` survives a prompt-only patch |
| T11 | Stale consent (TOCTOU) | `plan_digest` / `expect_plan`; deletions require it | tests: mismatch 409, missing-with-deletes 409, nothing applied |
| T12 | Cost/DoS on the user's credential (A4) | ownership gate; one run per project; turn/size/output caps; 240s kill | tests: busy 429, caps 400 |
| T13 | Cross-tenant use (A3) | API `ProjectScope` before forwarding; engine route is engine-secret realm | test: unauthenticated API call 401 |
| T14 | Builder output renders as markup | rendered as React text; no HTML sink | component test (existing) |

Residuals, stated so nobody reads a stronger claim into this:
- **Shared uid (§2, redteam 037).** A board agent can read `creds/builder/wheel-token`, as it can
  read every node's credential dir today. Per-node uids fix both; this adds no new class.
- **Human review is the last line against a *plausible* hostile proposal.** The consent UI makes
  every change explicit, but it cannot judge intent.
- **Real-CLI flags are verified against the fake only.** `--tools ""`,
  `--no-session-persistence`, `--include-partial-messages`, `--system-prompt-file` and
  `--strict-mcp-config` are the documented Claude Code 2.1 print-mode flags, but only the
  opt-in `make test-live` proves them against the real binary. **Run it before enabling this in
  production.**
- **Process-mode hosts buffer the stream.** When the engine is on a unix socket,
  `wheel-host`'s `forward_over_socket` buffers the whole response, so the stream arrives in one
  piece at the end. It is correct, but not incremental. Fixing it is a `wheel-host` change,
  listed as follow-up.
- **Not atomic.** The engine-side batch route is still the only true atomicity. This change
  makes the destructive half conditional, so a partial apply cannot also destroy.

## 5. The web runner, and the server-side branch

- **New modules, leaving `api.ts`/`events.ts`/`local-auth.ts` mostly untouched:**
  - `src/lib/builder-stream.ts`: a pure SSE frame parser plus classification of pre-stream error
    bodies. It encodes rules, so it is in the vitest coverage include.
  - `src/lib/builder-client.ts`: transport, `makeBuilderRunner`, and the builder credential
    calls. **This one function is the only thing that changes when the server-side branch
    lands.** Today it sends `x-auth-token` to `${apiBaseUrl()}`; after, it sends a same-origin
    `/api/wheel/v1/projects/{id}/builder/turns` with the session cookie.
  - `src/components/builder/builder-session.tsx`: the runner, credential handling and mode.
- **`api.ts` changes by one parameter.** `applyBoard` takes the grants and `expect_plan`.
- **Mounting:**
  - A running project with an **empty** board shows the builder conversation instead of the
    empty grid, with a "start with an empty board" escape.
  - A populated board gets **Improve with the builder**, which opens the same session in improve
    mode.
  - Creating a project now navigates to its board.
- **Depends on `web/server-side-api`** (not pushed at the time of writing, checked with
  `git fetch`):
  - The Next `/api/wheel/[...path]` proxy must **stream** `text/event-stream` bodies. It must not
    buffer them, and must not apply a short total timeout: builder turns run up to 240s.
  - Both `builder-client.ts` and `applyBoard` switch to the cookie transport.

  I will rebase onto that branch when it is pushed; until then the transport is one function.

## 6. RULINGS requested

1. **Explicit removals** (§2), not omission-as-delete.
2. **Credential default order:** explicit, then the builder's own store, then `needs_auth` (§1).
   Also, that paste-code OAuth for the builder store is out.
3. **Deletions require `expect_plan`**: server-enforced for deletes only; optional otherwise.
4. **Renames are refused** in improve rather than supported.
