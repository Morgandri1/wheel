# Runbook — rotating `WHEEL_ENGINE_SECRET` for a project

Owner: API. Written 2026-09-07, **before it was needed**, at PM's request, so it is not authored at
speed during an incident. Provoked by finding 043's post-close remediation (`3d18b01` on
`redteam/main`).

**Status: verified from source, NOT rehearsed against production.** Every mechanical claim below was
checked in the code and is cited. No step has been executed against a live project. The first real
run should be on a throwaway project, not on `wheel-dev`.

## What this covers, and what it does not

`WHEEL_ENGINE_SECRET` is the bearer the host presents to a project's engine control plane. Holding it
is control-plane godmode for that project: it bypasses the wire matrix entirely.

This runbook rotates **that value only**. It does **not**:

- rotate `WHEEL_VAULT_KEY` (expensive — it encrypts vault values at rest, so a true rotation is a
  decrypt-all/re-encrypt-all migration that does not exist yet);
- rotate the **vault values** — the Anthropic OAuth token, the Codex key, any other account
  credential. 043 is explicit that these are the compromised things and that a new `vault_key` does
  not help a credential whose plaintext already leaked. **Those must be regenerated at the provider.**
  That is the expensive, important half and it is not this document.

## Precondition — do not rotate early

**Rotate only AFTER the carrier is closed** — specifically after #17 scrubs `WHEEL_ENGINE_SECRET` and
`WHEEL_VAULT_KEY` from the engine's environ. Rotating into a still-open exposure re-exposes the new
secret immediately and costs a rotation for nothing.

043 confirms rotation is **not** gated on per-node uids, so it does not wait for that larger work.

## The one thing that makes this harder than it looks

`project_secrets.engine_secret_enc` is **AES-256-GCM sealed** with the API's `API_MASTER_KEY`
(`crypto::seal`, `crypto.rs:75` — random 12-byte nonce prepended to the ciphertext). So:

> **You cannot rotate this from `psql`.** There is no SQL expression that produces a valid sealed
> blob. Writing a plaintext secret into that column produces a project whose secrets cannot be
> decrypted, and `load_secrets` fails on every start.

A tool is required, and it now exists: `cargo run -p wheel-api --example rotate-engine-secret`.
See step 2.

## The mechanism, which is simpler than expected

The reason this is a two-step rotation rather than a careful dance is that **both `start` and
`restart` call `reprovision` first** (`routes/projects.rs:255` and `:310`), which re-`PUT`s the
project's secrets to the host before the engine is spawned. The host's `upsert` rotates
`engine_secret` and `vault_key` on conflict while preserving `desired_running` and `uid_base`
(`store.rs:66-75`).

So one `restart` updates the host's record **and** respawns the engine with the new value, together.

There is therefore **no window** in which the host holds a new secret while the engine still runs
with the old one — which is the failure I expected to have to document, and it is already closed by
`reprovision`. If `reprovision` is ever removed from the start path, this runbook becomes wrong;
that is the thing to re-check before trusting it.

## Procedure

1. **Confirm #17 has landed.** If it has not, stop.

2. **Reseal a new secret** into `project_secrets`. Dry run first — it writes nothing without
   `--apply`, and it verifies it can decrypt the CURRENT value before it writes anything, so a
   master key that is wrong for this database stops here instead of destroying a good row.

   ```
   DATABASE_URL=… API_MASTER_KEY=… \
     cargo run -p wheel-api --example rotate-engine-secret -- <project-uuid>
   #   … then, once the dry run reports the row present and decryptable:
   DATABASE_URL=… API_MASTER_KEY=… \
     cargo run -p wheel-api --example rotate-engine-secret -- <project-uuid> --apply
   ```

   It prints no secret in either mode, touches only `engine_secret_enc`, and restarts nothing.

3. **Restart the project**, which re-provisions the host and respawns the engine with the new value:

   ```
   curl -fsS -X POST "$API/v1/projects/$PROJECT_ID/restart" \
     -H "x-auth-token: $TOKEN" -H "x-project-id: $PROJECT_ID"
   ```

4. **Verify the board answers**, which exercises API → host → engine with the new bearer end to end:

   ```
   curl -fsS "$API/v1/projects/$PROJECT_ID/engine/v1/board" \
     -H "x-auth-token: $TOKEN" -H "x-project-id: $PROJECT_ID" | head -c 200
   ```

   A `200` with nodes means the host and the engine agree on the new secret. A `502` means they do
   not: the engine is running with the old value and the host is presenting the new one.

5. **Agents:** the engine restart stops every child. Parked agents resume on their next message;
   `run_on_startup` agents come back parked, not running (§2). Expect the board to show `stopped`
   briefly.

## If it goes wrong

The old secret is gone once step 2 overwrites the column, so there is no rollback to it. The recovery
is forward: reseal **another** new secret and restart again. Since `reprovision` re-sends on every
start, any state where host and engine disagree is fixed by one more restart — the engine is spawned
with whatever the API currently holds, so the API's copy is authoritative by construction.

The unrecoverable mistake is writing an **unsealed** value into `engine_secret_enc`: `load_secrets`
then fails and `start` returns `500` for that project until the column holds a valid sealed blob
again. Never write that column by hand.

## The tool, and what still gates its use

`crates/wheel-api/examples/rotate-engine-secret.rs` is a thin main over
`wheel_api::admin::rotate_engine_secret`. The logic lives in the library so it can be tested as a
function; the example only parses arguments and prints.

Guarantees, each with a test that fails when the guard is removed:

- **A dry run does not write.** `--apply` is the only path that touches the row.
- **A wrong `API_MASTER_KEY` refuses.** The current value is decrypted first, and a key that cannot
  open it is rejected rather than used to overwrite a good row with a value the API could never
  decrypt — the unrecoverable mistake named above.
- **An unknown project is an error naming it**, not a silent no-op.

Both guards are proven by mutation rather than by reading: remove the early return or the decrypt
check, and the corresponding test goes red.

**What still gates its use.** PM holds the ruling on running it against a real project, and
ADVERSARY reviews before the first `--apply` — the same bar the probe-prune script cleared, because
this is the same class: a tool whose failure mode is an outage. It has never been run against
anything but a throwaway SQLite database.

And the rotation it enables is itself gated on #17 closing the environ carrier first (see
"Precondition"). Having the tool does not move that.
