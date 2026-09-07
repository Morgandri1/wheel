# SSRF classifier battery (t_ip_classifier_battery.rs)

Proves `wheel_core::ip_is_denied` / `host_is_denied` deny every private/metadata/
IPv6-embedded-IPv4 form (6to4, NAT64, Teredo XOR-client, ::ffff mapped, ::compat)
and allow genuine public addresses. RUN, 33/33 pass (see finding notes).

Run as a standalone cargo bin with a path dep on wheel-core:
  cargo new /tmp/ssrf && cd /tmp/ssrf
  # Cargo.toml: wheel-core = { path = ".../crates/wheel-core" }  + empty [workspace]
  cp t_ip_classifier_battery.rs src/main.rs && cargo run

QA: the cleaner home is wheel-core's own #[cfg(test)] mod (no path-dep needed).
