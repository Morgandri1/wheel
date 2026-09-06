# 040 — The catch_unwind quarantine belt is never exercised by a test; a regression would be silent

- **Severity:** Low (defense-in-depth not proven; not exploitable today). Owner: SDK/Engine (add a
  panicking test-harness) + QA (keep it in the gate). Boundary TB4 (engine ↔ child delivery). Directly
  corrects a claim I made in **035** — I credited the `catch_unwind` quarantine as a verified belt; running
  the tests shows it is verified by none.
- **Status:** CONFIRMED by RUNNING, not reading. Evidence below is the exact commands and their output.

## Why this exists
PM's challenge was "I do not want 'looks right', I want what you ran." So I ran the tests my 035 verdict
rested on, and one of my own claims did not survive it.

## What I actually ran (commands + results, from redteam worktree at origin/main e4cd743)

```
$ cargo test -p wheel-core --test envelope
running 18 tests ... test result: ok. 18 passed; 0 failed   (exit 0)
  incl. the_body_that_took_the_board_offline_escapes_instead_of_panicking
        a_multibyte_char_eleven_bytes_after_a_less_than
        no_truncation_point_in_multibyte_content_can_panic_the_escaper
        no_stored_body_can_stop_the_engine_from_starting
        body_survives_the_envelope_byte_for_byte / envelope_is_byte_exact

$ cargo test -p wheel-engine --lib api::ingress
running 6 tests ... test result: ok. 6 passed; 0 failed   (exit 0)
  incl. ingress_delivers_only_through_the_one_envelope_sink   (SOURCE-GREP tripwire)
        a_hit_is_attributed_to_the_endpoint_and_never_to_the_user (SOURCE-GREP tripwire)
        the_rate_limiter_stops_a_caller_past_the_window_budget  (isolation — confirms 039's premise)
```

**What those RUNS actually prove, stated honestly by kind:**
- **The sink is behaviorally safe.** The 18 wheel-core tests drive real bytes — the exact production body,
  the byte-11 boundary, every multibyte truncation point — through `escape_envelope_body` and observe no
  panic, and `no_stored_body_can_stop_the_engine_from_starting` proves boot-survival. This is behavioral and
  it is the load-bearing proof for 034/035. Ran, passed.
- **Ingress reaches that sink** is proven **structurally**, not behaviorally. `ingress_delivers_only_through_
  the_one_envelope_sink` is a `include_str!` + `assert!(!code.contains("escape_envelope_body"))` grep of the
  module — a tripwire that fails if a future edit hand-rolls an envelope beside the sink. There is no test
  that pushes a live poison body through the ingress HTTP handler into a real queue and reads it back. The
  "ingress body is safe" conclusion is therefore a COMPOSITION: tripwire (ingress → `messages::enqueue`) +
  the 18 sink tests (`enqueue` → `envelope` → escaper). Sound, but it is inference over two test kinds, not
  one end-to-end behavioral test. I state that rather than call it "verified behaviorally."

## The gap running found (the finding itself)
The belt I credited in 035 — `std::panic::catch_unwind` around `encode_turn` at
`crates/wheel-engine/src/supervisor/mod.rs:686-702`, which on panic calls `messages::quarantine` and returns
`Ok(())` so the delivery loop survives — **is exercised by no test.**

Verified by running:
- The only two `impl Harness` are `ClaudeDriver` (real, `harness/claude.rs:71`) and `ShimDriver` (test-only,
  `supervisor/mod.rs:1296`). `ShimDriver::encode_turn` is `format!("{envelope}\n")` — it cannot panic. No
  test-double panics in `encode_turn`.
- `grep` for a panicking encoder in the crate returns nothing.
- The three `quarantine` tests (`db/messages.rs:634,655,683`) test the **DB primitive** — that a quarantined
  row is never re-offered and records its reason. They never drive the `catch_unwind` at the call site.

So the *integration* — panic in encode → caught → quarantined → loop returns `Ok` → next message delivered —
has zero coverage. If a refactor dropped the `catch_unwind`, or if `panic = "abort"` ever crept into a
profile (PM has flagged `panic = "unwind"` as load-bearing precisely for this belt), **no test would fail**,
and the belt that is our only defense against the NEXT poison sink would be silently gone. The escaper is
fixed today, so this is not exploitable now; it is a regression surface on a load-bearing safety mechanism.

## Proposed fix (SDK owns; a diff sketch, not applied — I do not edit product code)
Add a panicking test harness and one delivery-loop test:

```rust
// in supervisor/mod.rs #[cfg(test)]
struct PanicShim { program: String }
impl Harness for PanicShim {
    fn program(&self) -> &str { &self.program }
    fn argv(&self, _: &SpawnSpec) -> Vec<std::ffi::OsString> { Vec::new() }
    fn env(&self, _: &SpawnSpec) -> Vec<(String, String)> { Vec::new() }
    fn encode_turn(&self, _envelope: &str) -> String { panic!("simulated NEW poison sink") }
    fn parse_line(&self, l: &str) -> HarnessEvent { ClaudeDriver.parse_line(l) }
    fn classify_startup_failure(&self, _c: Option<i32>, s: &str) -> StartupFailure {
        ClaudeDriver.classify_startup_failure(None, s)
    }
}

#[tokio::test]
async fn a_panicking_encoder_quarantines_the_message_and_the_loop_survives() {
    // build a supervisor over PanicShim with two queued messages;
    // deliver; assert msg#1 -> Quarantined (state + reason recorded),
    // the delivery fn returned Ok, and msg#2 was then delivered.
}
```

This makes the belt a behavior a test owns, not code that "looks right." QA: add it to the gate so
`panic = "abort"` or a dropped `catch_unwind` turns the build red rather than shipping a silent dead-board
regression.

## Cross-refs
- **035** — I have amended its "links 1-4 fixed (034 escaper + `catch_unwind` quarantine)" line: the escaper
  fix is behaviorally proven (18 tests, run); the quarantine belt is PRESENT in source but NOT test-exercised
  (this finding). The chain conclusion is unchanged — the escaper alone closes the known poison — but the
  belt is a claim I should not have called verified.
- **A10 / Cargo.toml** — `panic = "unwind"` in `[profile.release]` is load-bearing for this belt; 040 is the
  test that would catch its removal.
