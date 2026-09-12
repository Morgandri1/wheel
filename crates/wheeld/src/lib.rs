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
pub mod guard;
pub mod supervise;
pub mod tokens;
pub mod update;

pub use config::Settings;

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;

/// Boot the whole product in this process.
///
/// The order is the contract: the host must be listening and its projects reconciled before the API
/// can provision anything, and the API must know the host's address, which is only assigned once
/// its listener is bound. So the host listener comes first, then the environment the API reads.
pub async fn run(settings: Settings) -> Result<()> {
    run_with_updates(settings, None).await
}

/// As [`run`], with the update lane already settled by [`cli_main`].
///
/// The lane is built BEFORE the runtime, not here: see `cli_main` for why a rollback that waited
/// until this function could not undo a binary that crashes on its way to it.
pub async fn run_with_updates(settings: Settings, lane: Option<Arc<update::Lane>>) -> Result<()> {
    let data_dir = supervise::prepare_data_dir(&settings.data_dir)?;
    let keys = supervise::Keys::load_or_create(&data_dir)?;

    let host = start_host(&data_dir, &keys, lane.as_ref().map(|l| l.hook())).await?;

    // An update that has passed every gate and waited for the board to go quiet arrives here. The
    // API stops serving, and then the SAME shutdown a SIGTERM runs stops every agent.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<update::Ready>();
    let _no_update = match lane.clone() {
        Some(lane) => {
            tokio::spawn(async move {
                let _ = ready_tx.send(lane.ready().await);
            });
            None
        }
        // Held, not dropped: a dropped sender resolves its receiver, which would read as "an
        // update is ready" on a deployment that has no updater at all.
        None => Some(ready_tx),
    };
    if let Some(lane) = lane.clone() {
        confirm_health_when_serving(&settings.bind, lane);
    }

    let installing = Arc::new(std::sync::Mutex::new(None));
    let served = {
        let installing = installing.clone();
        serve_api(&settings.bind, &data_dir, async move {
            match ready_rx.await {
                Ok(ready) => *installing.lock().unwrap() = Some(ready),
                Err(_) => std::future::pending().await,
            }
        })
        .await
    };
    // After the API stops taking requests, before the process exits: every engine stops its
    // agents, whatever ended serving. Nothing this daemon started may outlive it.
    host.sandbox.shutdown_all().await;

    let ready = installing.lock().unwrap().take();
    if let (Some(lane), Some(ready)) = (lane, ready) {
        // Swaps the binaries and restarts onto them; only returns if that failed.
        lane.install_and_restart(&ready)?;
    }
    served
}

/// Tell the lane this build works, once it answers its own health check.
///
/// The new binary proving itself, rather than anything outside deciding: what matters is that this
/// process serves, and the honest test of that is a request it answers.
fn confirm_health_when_serving(bind: &str, lane: Arc<update::Lane>) {
    if !lane.on_probation() {
        return;
    }
    let port = bind.rsplit_once(':').map(|(_, p)| p.to_string());
    tokio::spawn(async move {
        let Some(port) = port else { return };
        let url = format!("http://127.0.0.1:{port}/healthz");
        let client = reqwest::Client::new();
        loop {
            if client
                .get(&url)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                lane.confirm_healthy();
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    });
}

/// The sandbox host, serving, and the engines behind it.
pub struct Host {
    /// The loopback URL the API reaches the host on.
    pub url: String,
    pub sandbox: Arc<embedded::EmbeddedSandbox>,
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
    // Settled before the runtime exists, and before anything else can fail. A build the last
    // update installed that never proved healthy is rolled back HERE, so the window in which a
    // broken binary cannot undo itself is only argument parsing and this call — not the tokio
    // runtime, the data directory, the API's configuration or its migrations, every one of which
    // can fail on a bad build and none of which could then reach a rollback
    // (docs/proposals/auto-update.md, "Rollback").
    let lane = match &action {
        config::Action::Run(settings) => {
            let data_dir = supervise::prepare_data_dir(&settings.data_dir)?;
            update::Lane::start(&data_dir)?.map(Arc::new)
        }
        _ => None,
    };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building the tokio runtime")?
        .block_on(dispatch_with(action, lane))
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
    dispatch_with(action, None).await
}

/// [`dispatch`], carrying the update lane `cli_main` already settled.
pub async fn dispatch_with(action: config::Action, lane: Option<Arc<update::Lane>>) -> Result<()> {
    match action {
        config::Action::PrintUsage => {
            print!("{}", config::USAGE);
            Ok(())
        }
        config::Action::PrintVersion => {
            // The commit is baked in at compile time, as the engine's is: an image built with
            // --build-arg GIT_SHA names the commit it was built from, and a cargo build says so.
            println!(
                "wheeld {} ({})",
                env!("CARGO_PKG_VERSION"),
                option_env!("WHEEL_BUILD_SHA").unwrap_or("unknown")
            );
            Ok(())
        }
        config::Action::Run(settings) => run_with_updates(settings, lane).await,
        config::Action::Update {
            data_dir,
            status_only,
        } => update::from_operator(&data_dir, status_only, &mut std::io::stdout()),
        config::Action::Token { data_dir, command } => {
            tokens::run(
                command,
                &data_dir,
                &mut std::io::stdout(),
                &mut std::io::stderr(),
            )
            .await
        }
    }
}

/// Bind the sandbox host, reconcile it, and start serving it in the background.
///
/// Split out of `run` because it is the half of the composition that owns no database: a test can
/// drive the whole host — router, store, embedded engines — without standing up Postgres, and what
/// it exercises is the real wiring rather than a rehearsal of it.
///
/// Returns the loopback URL the API should use, and the engines, which the caller stops.
pub async fn start_host(
    data_dir: &std::path::Path,
    keys: &supervise::Keys,
    update: Option<Arc<dyn wheel_engine::update::UpdateHook>>,
) -> Result<Host> {
    // Loopback only, on a port the OS picks. Nothing outside this machine may reach the host: it
    // is the half of the process that can start and stop any project's engine.
    let host_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding the host listener")?;
    let host_addr = host_listener.local_addr()?;
    let host_url = format!("http://{host_addr}");

    supervise::apply_defaults(&supervise::composed_env(data_dir, keys, &host_url));

    let (host_state, sandbox) = build_host_state(data_dir, update)?;
    wheel_host::reconcile_on_boot(&host_state).await;
    tokio::spawn(async move {
        if let Err(e) = wheel_host::serve_on(host_listener, host_state).await {
            tracing::error!(error = %format_args!("{e:#}"), "the sandbox host stopped");
        }
    });
    tracing::info!(%host_url, "sandbox host ready");
    Ok(Host {
        url: host_url,
        sandbox,
    })
}

/// The host, with engines embedded rather than spawned.
fn build_host_state(
    data_dir: &std::path::Path,
    update: Option<Arc<dyn wheel_engine::update::UpdateHook>>,
) -> Result<(wheel_host::HostState, Arc<embedded::EmbeddedSandbox>)> {
    let cfg = wheel_host::config::Config::from_env().context("host configuration")?;
    let store = Arc::new(wheel_host::store::Store::open(
        &data_dir.join("host.db").display().to_string(),
    )?);
    let sandbox = Arc::new(
        embedded::EmbeddedSandbox::for_data_dir(
            data_dir.to_path_buf(),
            std::time::Duration::from_secs(cfg.start_timeout_secs),
        )?
        .with_update(update),
    );
    let state = wheel_host::HostState {
        cfg,
        sandbox: sandbox.clone(),
        store,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building the host http client")?,
        auth_limiter: Arc::new(wheel_host::auth_limit::AuthLimiter::new(30)),
        ready: wheel_host::Readiness::serving_from_start(),
    };
    Ok((state, sandbox))
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

/// Where this daemon says it is reached — the issuer of its sessions and the base of every ingress
/// URL — when `PUBLIC_BASE_URL` does not say. `localhost` for loopback and wildcard binds, which is
/// what every earlier install used, so their sessions keep their issuer across the upgrade.
fn default_public_base(bind: &str) -> String {
    let (host, port) = bind.rsplit_once(':').unwrap_or((bind, "8080"));
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let local = host.is_empty()
        || host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified());
    if local {
        format!("http://localhost:{port}")
    } else if host.contains(':') {
        format!("http://[{host}]:{port}")
    } else {
        format!("http://{host}:{port}")
    }
}

async fn serve_api(
    bind: &str,
    data_dir: &Path,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let guard = Arc::new(guard::Guard::from_env(bind));
    let trusted = Arc::new(
        wheel_api::http::client_ip::TrustedProxies::from_env()
            .map_err(anyhow::Error::msg)
            .context("trusted proxies")?,
    );
    supervise::apply_defaults(&[("PUBLIC_BASE_URL", default_public_base(bind))]);
    let cfg = wheel_api::config::Config::from_env().context("api configuration")?;
    if cfg.signup == wheel_api::config::SignupPolicy::Closed {
        tracing::info!(
            "signup is closed (WHEEL_SIGNUP=open opens it); the owner adds accounts with POST /v1/auth/users"
        );
    }
    let http = wheel_api::boot::http_client(&cfg)?;
    let db = wheel_api::boot::connect_and_migrate(&cfg).await?;
    if let Some(path) = tokens::bootstrap_operator(&db, data_dir).await? {
        tracing::info!(
            path = %path.display(),
            "wrote an operator token for the token-only owner account; send it as x-auth-token"
        );
    }
    let origins = wheel_api::boot::cors_origins_from_env();
    if !origins.is_empty() {
        tracing::info!(
            origins = %origins.join(","),
            "CORS_ALLOWED_ORIGINS lets pages on these origins call the API from a browser"
        );
    }
    let state = wheel_api::boot::build_state(cfg, db.clone(), http).await;
    wheel_api::boot::spawn_maintenance(db, std::time::Duration::from_secs(60));

    let app = wheel_api::build_router(state, &origins)
        .layer(axum::middleware::from_fn_with_state(guard, guard::check))
        .layer(axum::middleware::from_fn_with_state(
            trusted,
            wheel_api::http::client_ip::resolve,
        ));
    if let Some(warning) = guard::exposure(bind) {
        tracing::warn!("{warning}");
    }
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    // The real address, not a guessed one: --bind exists, and telling someone to open a port the
    // process is not listening on is the least helpful possible first line.
    tracing::info!("wheel is ready — open http://{}", displayable(bind));
    // With connect info: the peer address is what decides whether X-Forwarded-For is believed.
    let (asked, stop_asked) = tokio::sync::oneshot::channel::<()>();
    let serving = std::future::IntoFuture::into_future(
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            // Either the operator stopping this daemon, or an update ready to replace it.
            tokio::select! {
                () = stop_requested() => {}
                () = stop => tracing::info!("stopping to install an update"),
            }
            let _ = asked.send(());
        }),
    );
    tokio::pin!(serving);
    // A stuck connection must not keep the engines, and their agents, waiting behind it.
    tokio::select! {
        result = &mut serving => result?,
        () = async {
            match stop_asked.await {
                Ok(()) => tokio::time::sleep(API_DRAIN).await,
                Err(_) => std::future::pending().await,
            }
        } => tracing::warn!("requests were still open {API_DRAIN:?} after the stop signal; stopping without them"),
    }
    Ok(())
}

/// How long the API drains open requests once asked to stop, before the engines are stopped anyway.
const API_DRAIN: std::time::Duration = std::time::Duration::from_secs(3);

/// Resolves when the daemon has been asked to stop.
///
/// A person runs `wheeld` in a terminal and stops it with ctrl-c, and a service manager stops it
/// with SIGTERM; either must end the process, and only after every engine has stopped its agents.
/// This is the one signal handler in the process: embedded engines are stopped by `run`, never by
/// a signal of their own.
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

    /// The issuer of every session and the base of every ingress URL. It must not move for an
    /// install that has always been `http://localhost:8080`, or an upgrade logs everyone out.
    #[test]
    fn the_public_base_defaults_to_where_this_daemon_is_reached() {
        assert_eq!(
            default_public_base("127.0.0.1:8080"),
            "http://localhost:8080"
        );
        assert_eq!(default_public_base("0.0.0.0:9000"), "http://localhost:9000");
        assert_eq!(default_public_base("[::1]:8080"), "http://localhost:8080");
        assert_eq!(
            default_public_base("localhost:8081"),
            "http://localhost:8081"
        );
        assert_eq!(
            default_public_base("192.168.1.5:8080"),
            "http://192.168.1.5:8080"
        );
        assert_eq!(
            default_public_base("[2001:db8::1]:80"),
            "http://[2001:db8::1]:80"
        );
        assert_eq!(default_public_base("box.lan:8080"), "http://box.lan:8080");
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
