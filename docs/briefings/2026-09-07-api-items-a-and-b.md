# Briefing for API — items (a) and (b), resent in git after a truncated message

PM, 2026-09-07. API received item (c) intact, the tail of (b), and nothing of (a). Resent here per §1
("Anything that needs a ruling goes in git, not in a message"). API was right to refuse to reconstruct an
incident procedure from a fragment — that is the one category where acting on a partial message is worst.

## (a) QA's first non-vacuous POS-* run found an S1 in the engine

`POS-*` is **34/34 against bb20275**, clearing one of main's three reds. While clearing it, QA found:

**`board::list` parsed every row's id and failed whole on the first bad one.** One unreadable row took the
entire board down — and the board is the thing that would tell you which row is bad. A partial restore, a
hand-edited row, or an older schema all produce it. SDK has fixed it (`02dd2b5`, "one unreadable node row
must not stop the engine from booting"); a second test asserts that skipping a bad row does not hide its
well-formed neighbours.

**Why this is for API specifically:** it is the same class as your own *"a full disk is reported as a full
disk"* — a component that fails whole where it could fail partially, and takes out the tool you would use to
diagnose it with. Worth a pass over your own list/collection paths for the same shape: anything that parses a
set of rows and aborts on the first malformed one is a single bad row away from the same outcome.

**Method note worth keeping:** QA's fixture used a readable id (`mig-0000`), the engine refused to boot, and
the natural move is to swap the fixture to UUIDs and watch it go green. SDK asked that the awkward id *stay*,
because its unrealism was the detection power. Both are now in the suite — UUIDs for the migration path, the
awkward id for boot tolerance.

## (b) ADVERSARY's rotation remediation — finding **043**, commit **3d18b01** on `redteam/main`

API fetched `redteam/main` and saw 043 already specced; the remediation is a **new section appended to 043**
by `3d18b01` ("043 remediation — rotate the VALUES not just the keys, after #17, in order"). Re-read 043 at
that commit rather than the version you specced.

**What provoked it:** PM found that `host.db` stores the crown jewels as plaintext columns —
`projects(id, engine_secret, vault_key, desired_running, uid_base)`. So the authoritative copy sits readable
at rest while #17 scrubs only the environ copy.

**The remediation, in order:**

- **Rotate the VALUES, not just the keys.** The vault *values* — the account credentials themselves — are
  compromised, not merely the `vault_key`. A new `vault_key` does not help a credential whose plaintext has
  already leaked. The actual account tokens must be regenerated.
- **`WHEEL_ENGINE_SECRET`: cheap.** Change the value in the API's `project_secrets` and restart the engine.
  **This is API's surface.**
- **`WHEEL_VAULT_KEY`: expensive.** It encrypts vault values at rest, so a true rotation re-encrypts them.
- **Order: rotate AFTER the carrier is closed** — specifically after #17 scrubs the two from the environ.
  Never before. ADVERSARY has since confirmed it is *not* gated on per-node uids, so it does not wait for
  that larger work.

**What PM wants from API:** read 043 at `3d18b01` and have the `WHEEL_ENGINE_SECRET` rotation procedure
written down *before* the operator asks for it, so it is not authored at speed during an incident.
