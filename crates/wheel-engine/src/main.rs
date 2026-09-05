//! `wheel-engine` — one process per project, inside its sandbox.

// The storage layer lands ahead of the control plane and supervisor that call
// it, so its constructors read as dead to the compiler until they are wired.
// It is fully unit-tested in the meantime.
// TODO remove when the supervisor is wired (M1)
#![allow(dead_code)]

use std::process::ExitCode;

mod config;
mod db;

use config::Config;

fn main() -> ExitCode {
    // Misconfiguration must fail loudly and immediately with a one-line reason:
    // the host reads a non-zero exit as "this sandbox is broken", which is far
    // better than an engine that boots half-configured and accepts traffic.
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wheel-engine: {e}");
            return ExitCode::from(2);
        }
    };

    if let Err(e) = run(cfg) {
        eprintln!("wheel-engine: {e:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run(cfg: Config) -> anyhow::Result<()> {
    init_tracing(cfg.json_logs);
    tracing::info!(project = %cfg.project_id, listen = %cfg.listen, "starting");

    let _conn = db::open(&cfg.db_path())?;
    tracing::info!(db = %cfg.db_path().display(), "database ready");

    // Control plane, supervisor and events WS land next.
    Ok(())
}

fn init_tracing(json: bool) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    if json {
        fmt().with_env_filter(filter).json().init();
    } else {
        fmt().with_env_filter(filter).init();
    }
}
