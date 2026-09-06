//! The Wheel engine as a library.
//!
//! One process per project is still the model (§2), but the process does not
//! have to be `wheel-engine`: `wheeld` embeds this to run an engine and a host
//! in one executable for local use. The binary in this crate is a thin wrapper
//! around [`serve`], so there is exactly one implementation of what an engine
//! does and no second copy to drift.

// The supervisor lands next and is the caller for several storage helpers, so
// they read as dead to the compiler meanwhile. They are unit-tested today.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

pub mod api;
pub mod auth;
pub mod caps;
pub mod config;
pub mod db;
pub mod events;
pub mod harness;
pub mod mcp;
pub mod oauth;
pub mod peercred;
pub mod supervisor;
pub mod tools;
pub mod vault;

use anyhow::Context;
pub use config::Config;
use wheel_core::ListenAddr;

/// Run an engine until it is asked to stop.
///
/// The caller owns the runtime, so an embedder can host this alongside other
/// work rather than being handed a process.
pub async fn serve(cfg: Config) -> anyhow::Result<()> {
    // Logging belongs to whoever owns the PROCESS, not to each engine in it.
    // `wheeld` runs several of these together; if serve() installed a global
    // subscriber, the second engine would panic on a logging detail. The
    // binary wrapper installs one; an embedder uses whatever it already has.
    tracing::info!(project = %cfg.project_id, listen = %cfg.listen, "starting");

    // Said at boot, every boot, at warn level. If someone ever has to work out
    // why a tenant read another tenant's data, this line is what they will
    // find -- and if it is absent, the answer is not "shared uid".
    let isolation = wheel_core::UidIsolation::from_env();
    if isolation.is_shared() {
        tracing::warn!(
            isolation = isolation.as_str(),
            "{}",
            wheel_core::SHARED_UID_WARNING
        );
    }

    let conn = db::open(&cfg.db_path())?;
    tracing::info!(db = %cfg.db_path().display(), "database ready");

    let listen = cfg.listen.clone();
    let cfg = Arc::new(cfg);
    let db = Arc::new(Mutex::new(conn));
    let events = Arc::new(events::Bus::new());
    let state = api::AppState {
        supervisor: Arc::new(supervisor::Supervisor::new(
            cfg.clone(),
            db.clone(),
            events.clone(),
        )),
        cfg,
        db,
        events,
        logins: Arc::new(oauth::LoginSessions::default()),
    };
    // Before serving: agents configured to run on startup come up parked, and
    // any message left queued by the previous run resumes exactly the agents
    // that have work waiting.
    state.supervisor.start_configured_agents().await;

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
                // 0700: the directory is the first line of the tenant boundary,
                // and every project's socket dir sits on one shared kernel.
                std::fs::set_permissions(
                    parent,
                    std::os::unix::fs::PermissionsExt::from_mode(0o700),
                )?;
            }
            let listener = tokio::net::UnixListener::bind(&path)?;

            // Set the mode EXPLICITLY rather than inheriting it from the
            // umask. Connecting to a unix socket requires the write bit, so a
            // permissive umask would silently publish this engine's control
            // plane to every uid on the machine — and in process mode all of a
            // host's tenants share one kernel. Observed 0755 here by umask
            // accident; that must not be what we depend on.
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
                .context("restricting the engine socket to its own uid")?;

            // Second lock on the same door: the mode above says who MAY
            // connect, this says who DID. If the mode is ever loosened by a
            // umask, a chmod, or a host that recreates the directory, this is
            // what still refuses another tenant.
            let listener = peercred::PeerCredListener::new(listener);

            tracing::info!(path = %path.display(), mode = "0600", "listening");
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

/// Install a global tracing subscriber, if there is not one already.
///
/// Returns whether THIS call installed it. Uses `try_init` rather than `init`
/// because a global subscriber can only be set once per process: `wheeld` runs
/// several engines in one process, and the second one panicking on a logging
/// detail is not an acceptable way to find that out.
///
/// A library caller should generally not call this at all — logging belongs to
/// whoever owns the process. [`serve`] deliberately does not.
pub fn init_tracing(json: bool) -> bool {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    if json {
        fmt().with_env_filter(filter).json().try_init().is_ok()
    } else {
        fmt().with_env_filter(filter).try_init().is_ok()
    }
}

#[cfg(test)]
mod tests {
    /// A global subscriber can only be set once per process. `wheeld` runs
    /// several engines together, so a second call must be a no-op rather than
    /// a panic — the failure mode this replaced was the SECOND engine dying on
    /// a logging detail, which is a miserable thing to debug.
    #[test]
    fn installing_tracing_twice_is_not_a_panic() {
        let first = super::init_tracing(false);
        let second = super::init_tracing(false);
        // Whether THIS test installed it depends on what else ran first, so
        // the assertion is that the two calls disagree at most about who won —
        // never that either of them panicked.
        assert!(
            !(first && second),
            "only one call may claim to have installed it"
        );
        // And a third, for the same reason.
        super::init_tracing(true);
    }
}
