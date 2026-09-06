//! `wheeld` — api, host and engines in one executable (ARCHITECTURE M1.7).
//!
//! Wheel in production is three services on two machines. That is the right shape for multi-tenant
//! cloud and the wrong shape for someone who wants to try it, or hack on it, on a laptop. `wheeld`
//! runs the same code in one process with no configuration.
//!
//! It is a composition, not a fourth implementation. The API router, the host router and the engine
//! are the ones that ship; what changes is only how they are wired together.

pub mod config;
pub mod embedded;
pub mod supervise;

pub use config::Settings;

use anyhow::{Context, Result};
use std::sync::Arc;

/// Boot the whole product in this process.
///
/// The order is the contract: the host must be listening and its projects reconciled before the API
/// can provision anything, and the API must know the host's address, which is only assigned once
/// its listener is bound. So the host listener comes first, then the environment the API reads.
pub async fn run(settings: Settings) -> Result<()> {
    let data_dir = supervise::prepare_data_dir(&settings.data_dir)?;
    let keys = supervise::Keys::load_or_create(&data_dir)?;

    start_host(&data_dir, &keys).await?;
    serve_api(&settings.bind).await
}

/// Everything the binary does, so that `main` is a call and nothing else.
///
/// The runtime is built here rather than by `#[tokio::main]` for the same reason the dispatch is:
/// it is a decision — how many threads, which drivers — and decisions belong where they can be read
/// and changed in one place. `--help` and `--version` still get a runtime, because building one is
/// cheaper than a second code path that avoids it.
pub fn cli_main<I, T>(args: I) -> Result<()>
where
    I: IntoIterator<Item = T>,
    T: AsRef<str>,
{
    let action = config::Settings::parse(args)?;
    if matches!(action, config::Action::Run(_)) {
        init_tracing();
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building the tokio runtime")?
        .block_on(dispatch(action))
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wheeld=debug".into()),
        )
        .init();
}

/// What the binary does with a parsed command line.
///
/// Here rather than in `main` so it can be tested: `--help` and `--version` must print and exit
/// cleanly, and neither may start a server or touch a data directory as a side effect.
pub async fn dispatch(action: config::Action) -> Result<()> {
    match action {
        config::Action::PrintUsage => {
            print!("{}", config::USAGE);
            Ok(())
        }
        config::Action::PrintVersion => {
            println!("wheeld {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        config::Action::Run(settings) => run(settings).await,
    }
}

/// Bind the sandbox host, reconcile it, and start serving it in the background.
///
/// Split out of `run` because it is the half of the composition that owns no database: a test can
/// drive the whole host — router, store, embedded engines — without standing up Postgres, and what
/// it exercises is the real wiring rather than a rehearsal of it.
///
/// Returns the loopback URL the API should use.
pub async fn start_host(data_dir: &std::path::Path, keys: &supervise::Keys) -> Result<String> {
    // Loopback only, on a port the OS picks. Nothing outside this machine may reach the host: it
    // is the half of the process that can start and stop any project's engine.
    let host_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding the host listener")?;
    let host_addr = host_listener.local_addr()?;
    let host_url = format!("http://{host_addr}");

    supervise::apply_defaults(&supervise::composed_env(data_dir, keys, &host_url));

    let host_state = build_host_state(data_dir)?;
    wheel_host::reconcile_on_boot(&host_state).await;
    tokio::spawn(async move {
        if let Err(e) = wheel_host::serve_on(host_listener, host_state).await {
            tracing::error!(error = %format_args!("{e:#}"), "the sandbox host stopped");
        }
    });
    tracing::info!(%host_url, "sandbox host ready");
    Ok(host_url)
}

/// The host, with engines embedded rather than spawned.
fn build_host_state(data_dir: &std::path::Path) -> Result<wheel_host::HostState> {
    let cfg = wheel_host::config::Config::from_env().context("host configuration")?;
    let store = Arc::new(wheel_host::store::Store::open(
        &data_dir.join("host.db").display().to_string(),
    )?);
    let sandbox = Arc::new(embedded::EmbeddedSandbox::for_data_dir(
        data_dir.to_path_buf(),
        std::time::Duration::from_secs(cfg.start_timeout_secs),
    )?);
    Ok(wheel_host::HostState {
        cfg,
        sandbox,
        store,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building the host http client")?,
        auth_limiter: Arc::new(wheel_host::auth_limit::AuthLimiter::new(30)),
        ready: wheel_host::Readiness::serving_from_start(),
    })
}

/// `0.0.0.0:8080` is not an address a browser can open; say `localhost` instead.
fn displayable(bind: &str) -> String {
    match bind
        .strip_prefix("0.0.0.0:")
        .or_else(|| bind.strip_prefix("[::]:"))
    {
        Some(port) => format!("localhost:{port}"),
        None => bind.to_string(),
    }
}

async fn serve_api(bind: &str) -> Result<()> {
    let cfg = wheel_api::config::Config::from_env().context("api configuration")?;
    let http = wheel_api::boot::http_client(&cfg)?;
    let db = wheel_api::boot::connect_and_migrate(&cfg).await?;
    let origins = wheel_api::boot::cors_origins_from_env();
    let state = wheel_api::boot::build_state(cfg, db.clone(), http).await;
    wheel_api::boot::spawn_maintenance(db, std::time::Duration::from_secs(60));

    let app = wheel_api::build_router(state, &origins);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    // The real address, not a guessed one: --bind exists, and telling someone to open a port the
    // process is not listening on is the least helpful possible first line.
    tracing::info!("wheel is ready — open http://{}", displayable(bind));
    axum::serve(listener, app)
        .with_graceful_shutdown(stop_requested())
        .await?;
    Ok(())
}

/// Resolves when the daemon has been asked to stop.
///
/// A person runs `wheeld` in a terminal and stops it with ctrl-c, and a service manager stops it
/// with SIGTERM; either must end the process. The embedded engines install SIGTERM handlers of
/// their own for their clean shutdown, and a handled signal no longer terminates the process by
/// default — so without this, `wheeld` keeps serving something that has been told to go away.
async fn stop_requested() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "cannot listen for SIGTERM");
            return;
        }
    };
    let mut interrupt = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "cannot listen for SIGINT");
            return;
        }
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = interrupt.recv() => {}
    }
    tracing::info!("stopping");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `0.0.0.0` and `[::]` are what `--bind` accepts to listen on every interface; neither is
    /// something a person can type into a browser, so both are rewritten to `localhost`. Anything
    /// else — the address a browser actually opens — must pass through unchanged.
    #[test]
    fn unbindable_addresses_become_localhost() {
        assert_eq!(displayable("0.0.0.0:8080"), "localhost:8080");
        assert_eq!(displayable("[::]:8080"), "localhost:8080");
        assert_eq!(displayable("127.0.0.1:8080"), "127.0.0.1:8080");
        assert_eq!(displayable("localhost:8080"), "localhost:8080");
    }

    /// `stop_requested` is what makes ctrl-c and `docker stop`/systemd's SIGTERM actually end the
    /// process (see the doc comment above it). Driven directly, rather than only through the
    /// subprocess in `tests/shutdown.rs`: a real SIGTERM sent to *this* test binary is exactly the
    /// signal tokio's own listener is registered for, so once `signal()` has run, delivering it here
    /// exercises the same `term.recv()` branch that ctrl-c would — without needing a second process.
    #[tokio::test]
    async fn a_sigterm_resolves_stop_requested() {
        let waiting = tokio::spawn(stop_requested());
        // Give the signal handlers a moment to register before the signal is sent — otherwise it
        // could be delivered (and lost, since nothing is listening yet) before `stop_requested` gets
        // as far as `signal(SignalKind::terminate())`.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        unsafe { libc::kill(std::process::id() as i32, libc::SIGTERM) };

        tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
            .await
            .expect("stop_requested did not resolve within 5s of SIGTERM")
            .expect("the task panicked");
    }

    /// `--help` and `--version` must print and exit without starting a server or touching a data
    /// directory. A daemon that provisions state to answer `--version` is one nobody can safely ask.
    #[tokio::test]
    async fn help_and_version_do_nothing_but_print() {
        let before = std::env::var("STORE").ok();
        dispatch(config::Action::PrintUsage).await.unwrap();
        dispatch(config::Action::PrintVersion).await.unwrap();
        assert_eq!(
            std::env::var("STORE").ok(),
            before,
            "printing usage configured something"
        );
    }
}
