//! `wheel-engine` — one process per project, inside its sandbox.

// The supervisor lands next and is the caller for several storage helpers, so
// they read as dead to the compiler meanwhile. They are unit-tested today.
// TODO remove when the supervisor is wired (M1)
#![allow(dead_code)]

use std::{
    process::ExitCode,
    sync::{Arc, Mutex},
};

mod api;
mod config;
mod db;

use config::Config;
use wheel_core::ListenAddr;

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

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("wheel-engine: cannot start tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run(cfg)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wheel-engine: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cfg: Config) -> anyhow::Result<()> {
    init_tracing(cfg.json_logs);
    tracing::info!(project = %cfg.project_id, listen = %cfg.listen, "starting");

    let conn = db::open(&cfg.db_path())?;
    tracing::info!(db = %cfg.db_path().display(), "database ready");

    let listen = cfg.listen.clone();
    let state = api::AppState {
        cfg: Arc::new(cfg),
        db: Arc::new(Mutex::new(conn)),
    };
    let app = api::router(state);

    match listen {
        ListenAddr::Tcp(addr) => {
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            tracing::info!(%addr, "listening");
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await?;
        }
        ListenAddr::Unix(path) => {
            // The socket is the whole tenant boundary in process mode, so a
            // stale one is removed rather than inherited, and the parent
            // directory is expected to already be owned by the project uid.
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let listener = tokio::net::UnixListener::bind(&path)?;
            tracing::info!(path = %path.display(), "listening");
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await?;
            let _ = std::fs::remove_file(&path);
        }
    }

    tracing::info!("shutdown complete");
    Ok(())
}

/// SIGTERM is how the host stops a sandbox, so it must be a clean shutdown —
/// children stopped and sqlite flushed — not a kill.
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "cannot install SIGTERM handler");
            return;
        }
    };
    let ctrl_c = tokio::signal::ctrl_c();

    tokio::select! {
        _ = term.recv() => tracing::info!("SIGTERM received, shutting down"),
        _ = ctrl_c => tracing::info!("SIGINT received, shutting down"),
    }
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
