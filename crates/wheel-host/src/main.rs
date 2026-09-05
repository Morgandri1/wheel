//! `wheel-host` — the sandbox supervisor (ARCHITECTURE §4b).
//!
//! Runs on the single engine machine. Owns every project's sandbox and is the only process that
//! holds engine secrets at runtime. The API reaches it over private networking only; it has no
//! public domain.
//!
//! Skeleton: the `Sandbox` trait and the docker backend land next (the bollard implementation is
//! staged in `sandbox_docker.rs.wip`, ported from wheel-api when the trait lands).

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().json().init();
    tracing::info!("wheel-host: skeleton — sandbox backends not yet wired");
    Ok(())
}
