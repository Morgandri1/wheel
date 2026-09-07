# Dogfooding the cloud board — which board mutations the API actually exposes

API, 2026-09-07. Read from the route tables at `4f619c5`, plus one experiment run against
`wheel-core` (§4). **Read, not exercised against production** — I have not driven these as a
logged-in user on prod. Say the word and I will.

In git because two messages carrying this were lost in one session, one beheaded and one delivered
empty.

## How reachability works

The API is a pass-through. `ANY /v1/projects/{id}/engine/{*rest}` proxies every verb to the engine,
owner-checked and project-scoped, injecting the engine bearer. So "does the API expose it" reduces to
"does the ENGINE have the route" — with one exception, §3.1.

## 1. Reachable today, as an authenticated project-scoped call

| area | routes |
|---|---|
| board | `GET /v1/board` |
| nodes | `POST /v1/nodes` · `PATCH /v1/nodes/{id}` · `DELETE /v1/nodes/{id}` |
| wires | `POST /v1/wires` · `DELETE /v1/wires` |
| agents | `start` `stop` `restart` `clear` `send` `log` `inbox` `inbox/{message_id}` · auth `begin` `complete` `status` `clear` |
| vault | `GET /v1/vault/{id}` → `{"keys":[…]}` **names only** · `PUT /v1/vault/{id}/{key}` · `DELETE` |
| tables | `GET /v1/tables/{id}/rows` · `POST /v1/tables/{id}/query` (read-only SQL) |
| tools | `POST /v1/tools/import` · `/v1/tools/{id}/import` · `GET ops` · `POST call` |
| events | `WS /v1/events`, plus the API's `POST /v1/projects/{id}/ws-ticket` for browsers |

**The default-DENY wire matrix IS enforced**, at creation, not at use: `add_wire` → `check_wire`
(wheel-core), covered by `crates/wheel-core/tests/wire_matrix.rs`.

## 2. The specific question — workspaces, budget, idle_timeout_secs

**The API exposes these writes. The missing layer is the UI.** All three are real fields on
`AgentConfig` (`crates/wheel-core/src/node.rs`), each with a serde default:

```rust
pub idle_timeout_secs: Option<u32>,
pub budget: Option<Budget>,          // max_turns / max_usd
pub workspaces: Vec<Workspace>,
```

- `POST /v1/nodes` takes a **typed** `NodeConfig` (`#[serde(flatten)]`), so an agent can be created
  with all three in one call.
- `PATCH /v1/nodes/{id}` takes `config` as an **untyped `serde_json::Value`**, then wraps it as
  `{"type": <node type>, "config": …}` and deserializes into the typed config.

So "create a working wheel-dev agent" is not blocked by the API. It is blocked at the UI.

## 3. But PATCH is REPLACE, not merge — and it deletes silently

`patch_node` **replaces `node.config` wholesale**. Every omitted field takes its serde default. With
`deny_unknown_fields` set, an *unknown* field is a loud 400 — but an *omitted known* field is a
silent reset.

Proven by deserializing through `wheel-core` itself, not argued:

```
BEFORE  {"harness":"claude","system_prompt":"hi","run_on_startup":true,
         "idle_timeout_secs":900,"budget":{"max_turns":50},
         "workspaces":[{"path":"/data/ws/wheel"}]}
AFTER   {"harness":"claude","system_prompt":"hi","run_on_startup":false,
         "ephemeral_context":false}

  idle_timeout_secs: Some(900)            -> None
  budget:            Some({max_turns:50}) -> None
  workspaces:        Some([{path:…}])     -> None
  run_on_startup:    true                 -> false
```

**A UI that PATCHes only the fields it has controls for will erase exactly the fields it has no
controls for**, with a 200 and no warning. That includes `run_on_startup` flipping true → false.

This matters more than the missing controls: adding a system-prompt editor *without*
read-modify-write is actively destructive to an agent someone configured through the API. Whoever
builds those controls must send the whole config back, or PATCH needs merge semantics.

I have not decided which; it is SDK's handler and Web's caller. Flagging the trap, not the fix.

## 4. Not reachable at all

1. **Chest content — no routes exist.** Not `ls`, not blob get, not blob put, though §4 specifies all
   three. A chest node can be created and never looked inside. Chest access exists ONLY on the CLI
   plane (`/v1/cli/read|write|ls`), which is deliberately mounted outside the engine-secret layer
   because it authenticates with a per-**node** token. The proxy injects the **engine** secret, so
   the operator cannot borrow it. There is no operator-authenticated way in.
2. **Table rows are read-only.** `rows` + `query` exist; no upsert, no delete. Agents can write via
   the CLI plane; the operator cannot.
3. **Scripts cannot be run.** No control-plane route invokes a script; `wheel run` is CLI-plane only.
   Authorable, editable, not executable from the board.
4. **No endpoint test.** Nothing fires an endpoint locally, so operator test traffic and a
   provider's real traffic are indistinguishable.
5. **No agent interrupt.** §3c#12's deliberate turn-cancel is absent; only `stop`, which kills the
   process rather than cancelling the turn.

## 5. One contract deviation, not a UI blocker

§3 says the wire matrix is rejected at creation "by engine AND api". The API does not check it — it
is a pass-through, so enforcement is engine-side only. Nothing is unsafe today because the engine
refuses; but the second layer the contract asks for does not exist, and any future non-proxy path
would inherit no check.

## 6. What this means for dogfooding

Nodes, wires, agents, vault and tools are fully operable. **Chests and tables are half-built** —
visible as nodes, inert as data — and they are the two node types where the data *is* the point.
Scripts and endpoints are authorable but not exercisable.

For "create a working wheel-dev agent in the UI" specifically: the API is not the blocker, the UI is
— and §3 is the thing to fix *before* those controls ship, not after.
