# 034 — Poison pill: persisted, attacker-authored content that panics/hangs a re-parse at boot = PERMANENT DoS

- **Severity:** High (availability). A message an AGENT can send (or the operator, or an ingress body once
  endpoints land) panics the engine when it is escaped, and reconcile replays it every 30s → the project's
  board stays down PERMANENTLY across restarts. Instance took the operator's board down at 14:34. Owner:
  SDK/Engine (`crates/wheel-core/src/message.rs` + every persisted-content re-parse path). Boundary TB5.
- **Status:** Instance CONFIRMED in source; class PoC `redteam/pocs/poison/t_escape_bytepanic.mjs` (verbatim
  port of the escaper's slice condition; 4/4 poison inputs → off-boundary slice = Rust panic, 3/3 controls
  safe). Live path noted below.

## Instance
`escape_envelope_body` (message.rs:183-214) walks the body by BYTE index and, at a `<`, does
`body[name_at .. name_at + TAG.len()]` (`:197-198`, `TAG="agentprompt"`, 11 bytes) — a **string slice by byte
index**. If a multi-byte UTF-8 char straddles either bound, Rust panics ("byte index N is not a char
boundary"). A `<` (or `</`) followed by ~4+ multi-byte chars (em-dash `—` = 3 bytes; emoji = 4) does it. The
body-walk at `:206` (`body[i..]`) is safe because `i` advances by `len_utf8`; the bug is the fixed-width
lookahead slice at `:197-198`. This is what an em dash in an operator message hit today.

## Class (attack the class, not the instance)
Any PERSISTED, (agent/operator/ingress)-authored content that is re-parsed or re-rendered at boot/spawn and can
PANIC or HANG the code that reads it is a **poison pill**: because reconcile replays it (every 30s) and
`journal_mode`/rows survive restarts, the failure is PERMANENT, not transient — it converts a one-time bad
input into an indefinite outage. Message bodies are one carrier; §3 has several others.

## Blast radius (PM's (d))
- **Per-project, permanent.** By finding 032, the host's reconcile is fault-isolated per project (log-and-
  continue) and each project has its own engine + its own db. So an engine that panics on a poison message
  crash-loops ONE project's engine; the others reconcile fine. But that one project is down forever (replayed
  every boot) until the row is removed.
- **Cross-tenant (whole grid): NOT reachable today — and here is the guard.** A poison is only cross-tenant if
  the SHARED host process re-parses it. The host's `reconcile_on_boot` reads only API-GENERATED values from
  `host.db` (engine_secret, base64 vault_key, project uuid, uid_base) — NOT tenant message/ctx/table content —
  so a tenant cannot plant a poison the host parses. **REGRESSION GUARD:** the host must never parse tenant-
  authored content at boot (no project names, no summaries containing tenant data in its own reconcile path);
  the day it does, a one-tenant poison becomes a whole-grid DoS (032's "one host process for every tenant").
- Messages are per-project (an agent cannot msg across projects), so agent→agent poison stays in its own
  project. The operator and (future) ingress bodies are the other authors, still per-project.

## The four vectors
- **(a) parser panic** — CONFIRMED (the escaper). AUDIT the same byte-slice/`unwrap`/`expect`/`[i..j]`/
  deserialize patterns on every persisted-content path: ctx markdown render (injected into the preamble at
  every start), node-name validation, table-row read/`untyped`, tool-config parse, importer, `wheel query`
  result build. Any one is another poison carrier.
- **(b) hang/timeout** — a body WITHIN the write-time size cap but expensive to re-parse (or simply MANY
  queued messages) makes reconcile/startup TIME OUT rather than panic. "Never panic" does not bound TIME or
  MEMORY; a hang at reconcile is the same permanent-DoS outcome without a crash.
- **(c) non-message stores** — ctx blocks, table rows, node names, tool specs are re-read on every start
  (`ensure_node_tables`, the preamble injector, config load). A poison there is re-parsed at boot exactly like
  a message, and some (ctx→preamble) run before any turn.
- **(d) shared-process cross-tenant** — see blast radius: not reachable via the host today (guard it); not
  reachable across projects via messages (per-project engines).

## Where the rule "an engine must not die of message content — degrade to text, never panic" is INSUFFICIENT
The rule is right but under-scoped in five ways:
1. **Scope — it is not just messages.** It must cover ALL persisted attacker-influenced content re-read at
   boot/spawn: ctx markdown, table rows, node names, tool specs, endpoint paths, vault key names. The em-dash
   bug is in the message escaper; the next one will be in `ensure_tables` or the ctx injector.
2. **Failure mode — "never panic" misses HANG/OOM (vector b).** Add a TIME and MEMORY budget at reconcile and
   at every re-parse; enforce size caps at READ, not only at write (a row/message written before a cap
   existed, or under a different cap, is re-read at boot).
3. **The degrade/quarantine path must itself be poison-safe.** If "degrade to text" runs the same
   byte-slice/parse that panicked, it panics in the handler. It must be fuzz/property-tested against the
   hostile class (multi-byte at every offset, invalid UTF-8, lone surrogate, NUL, 0-length, max-length,
   deeply nested) — proven, not asserted.
4. **Extend to the HOST, or forbid it parsing tenant content.** The rule as written protects the per-project
   engine; the shared host is the one whose death is cross-tenant. Either the host obeys the same rule for any
   content it reads, or it is contractually forbidden from parsing tenant-authored content at boot (my guard
   above). State which.
5. **The general principle is QUARANTINE, not degrade-to-text.** "Degrade to text" fits the escaper; it is
   meaningless for a row that won't deserialize or a config that won't parse. The durable rule is: **a single
   poisoned persisted record is skipped, logged, and surfaced — the engine/host reaches a SERVING state
   without it, every boot, forever** — and the skip is deterministic so it doesn't re-fatal on replay. Text-
   degradation is one instance of quarantine; the rule should be stated as quarantine + a per-record fault
   boundary, so no single stored record can exceed its own scope or outlive a restart as an outage.

## Fix (instance + class)
Instance: iterate by `char_indices()` / match on a byte prefix without a `str` slice, or guard the slice with
`is_char_boundary`. Class: audit the paths in (a)/(c) for the same patterns; add the boot-time budget (b);
fuzz the escaper + every persisted re-parse with the hostile input class; adopt the quarantine rule (5).
