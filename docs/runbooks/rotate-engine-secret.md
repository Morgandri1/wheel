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

A tool is required. See "The gap" below — **as of writing, that tool does not exist.**

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

2. **Reseal a new secret** into `project_secrets` for the project. Requires `API_MASTER_KEY` and the
   API's own crypto — see "The gap".

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

## The gap — and what I recommend

**There is no rotation path in the API today.** Secrets are generated exactly once, at project create
(`routes/projects.rs:55`), and nothing else ever writes `project_secrets`. Step 2 above has no
supported way to be performed.

Options, cheapest first:

1. **A one-shot admin binary** in `wheel-api` (`examples/` or a `src/bin`) that reads `DATABASE_URL`
   and `API_MASTER_KEY` from the environment, generates a secret with `crypto::generate_secret`,
   seals it, and `UPDATE`s the one row. ~40 lines, reuses the crypto that already exists, and is not
   reachable from the running API.
2. **An owner-only endpoint**, `POST /v1/projects/:id/rotate-engine-secret`, which does steps 2 and 3
   in one call and cannot leave the two out of step. Better ergonomics; a larger surface, and it
   creates an authenticated way to disrupt a project.

I recommend **(1)**, and I have not built it. It mutates production secrets, which puts it in the
same class as the probe-prune script — a tool whose failure mode is an outage — and that class got
ADVERSARY review before its first `--apply`. I would rather it be reviewed than exist at 3am.

**PM: say the word and I will build (1) with a dry-run default.** Until then this runbook documents a
procedure whose step 2 is blocked, and it is better for that to be written down and visible than
discovered during an incident.
