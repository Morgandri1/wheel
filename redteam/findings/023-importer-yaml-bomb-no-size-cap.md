# 023 — Tool importer: YAML alias-expansion (billion-laughs) + no input size cap → import-path DoS

- **Severity:** Medium (DoS of the engine import path; when the route lands, a hostile spec can OOM/hang the
  engine, which serves the whole project sandbox). Owner: **SDK/Engine** (`crates/wheel-engine/src/tools/import.rs`).
- **Status:** FIXED & VERIFIED e2e (shipped with the routes @ 6c371c7): `parse_document` now enforces
  `MAX_DOCUMENT_BYTES` (execute/import.rs:57) AND rejects documents containing YAML anchors/aliases
  ("this YAML uses an anchor or alias (&a0)" — confirmed live via `POST /v1/tools/import`). Both gaps closed.
  Was: source-grounded, HIGH confidence, **not executed in-crate** (PyYAML absent on host; a
  bounded Rust harness or the live `POST /v1/tools/import` route will confirm — see below). No HTTP route yet.
- **Boundary:** TB7 (tool import). PM's named target: "malicious spec DoS on import (YAML bombs, loops)."

## What
`import()` → `parse_document(raw)` (`import.rs:54-61`) tries `serde_json::from_str` then
`serde_yaml::from_str::<Value>(raw)`. Two gaps:
1. **No input size cap on `raw`** anywhere in `import()`/`parse_document` — a 100 MB spec is parsed fully
   into memory. (The future `POST /v1/tools/import` body limit is the only backstop; it must exist and be
   small.)
2. **`serde_yaml = "0.9.34+deprecated"`** (libyaml-based) expands YAML **anchors/aliases** into a full
   `Value` with **no alias/node/expansion cap**. This is the billion-laughs / YAML-bomb class: a ~300-byte
   document expands to 10^8–10^9 nodes.
```yaml
a0: &a0 "lol"
a1: &a1 [*a0,*a0,*a0,*a0,*a0,*a0,*a0,*a0,*a0,*a0]
a2: &a2 [*a1,*a1,*a1,*a1,*a1,*a1,*a1,*a1,*a1,*a1]
# … a9 → 10^9 materialized nodes from a tiny input → OOM/CPU
top: *a9
```
Each added level multiplies node count ×10; a handful of levels exhausts memory or pins CPU. Nothing in
`import.rs` bounds it (contrast: `$ref` cycles ARE bounded — `resolve_ref` `for _ in 0..8`, `import.rs:230`,
so the $ref-loop DoS PM named is already defended; this is the OTHER half).

## Impact
When `POST /v1/tools/import` lands (it consumes exactly this parser), an operator paste OR an agent that can
drive import supplies a small hostile spec and OOMs/hangs the engine — a denial of service against the whole
per-project sandbox (the engine is one process per project). Also huge-but-flat specs (no aliases, just many
paths/operations) hit gap #1 with no operations-count cap either.

## Fix
- **Cap `raw` size** before parsing (e.g. ≤1 MiB; real specs are far smaller) — and set a small body limit on
  the import route.
- **Neutralise alias expansion**: `serde_yaml` 0.9 is deprecated and offers no expansion bound. Either
  reject documents that contain YAML aliases (`*anchor`) for untrusted input, or move to a parser/config that
  caps alias expansion / total node count, or deserialize under a bounded `spawn_blocking` with a memory
  watchdog. A node/element cap during normalization (`operations`, params) is a cheap second layer.
- Add a test: a small billion-laughs YAML must be REFUSED (size/alias/node cap), not materialised.

## Confirmation path (so this isn't taken on faith)
A standalone `serde_yaml = "0.9"` harness parsing the bomb above at increasing depth shows super-linear node
blowup from a tiny input (bound the harness to depth ≤5 / ~10^5 nodes so it can't OOM the shared host). I did
not run it in-session to avoid a heavy compile + any risk to the shared dev host; the behavior is
well-established for libyaml-based parsers and `import.rs` demonstrably applies no cap. Will confirm live the
moment the import route exists (bounded payload, per RoE).

## Verified-strong (credit SDK) — NOT a finding
`$ref` resolution is bounded to 8 hops (`resolve_ref`), so a `$ref` cycle terminates rather than looping —
the other DoS vector PM named is already closed. `serde_json` (the JSON path) enforces a recursion limit, so
deeply-nested JSON errors rather than overflowing; the YAML depth path is the softer one.
