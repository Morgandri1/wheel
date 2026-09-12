// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Cross-project path traversal through the API's proxies.
//!
//! The engine proxy and the public ingress both forward a caller's path beneath a host URL that
//! names one project. Appending it as a string let the URL parser decode an encoded `..` a second
//! time, so a request for project A could arrive at the host addressed to project B's engine. These
//! tests put a recording host behind the real router, seed two projects with different owners, and
//! prove that no spelling of a parent step reaches B, or reaches the host at all.

// Exercises the SQLite backend, so it exists only in a build that has one.
#![cfg(feature = "sqlite")]

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use wheel_api::config::{AuthMode, Config, Env, SignupPolicy};
use wheel_api::crypto::Secret;
use wheel_api::db::Db;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

/// `depth` parent steps and then `tail`, spelled every way a parser after axum's single decode
/// might turn back into `..` and `/`.
fn spellings(depth: usize, tail: &[&str]) -> Vec<(&'static str, String)> {
    let spell = |parent: &str, sep: &str| {
        let mut parts = vec![parent; depth];
        parts.extend_from_slice(tail);
        parts.join(sep)
    };
    vec![
        ("literal ..", spell("..", "/")),
        ("encoded %2e%2e", spell("%2e%2e", "/")),
        ("double-encoded %252e%252e", spell("%252e%252e", "/")),
        ("half-encoded .%2e", spell(".%2e", "/")),
        ("mixed-case %2E%2e", spell("%2E%2e", "/")),
        ("encoded slash %2f", spell("..", "%2f")),
        ("double-encoded slash %252f", spell("..", "%252f")),
        (
            "double-encoded dots and slash",
            spell("%252e%252e", "%252f"),
        ),
        ("encoded backslash %5c", spell("..", "%5c")),
        ("double-encoded backslash %255c", spell("..", "%255c")),
        ("tab before ..", spell("%09..", "/")),
    ]
}

/// Paths that climb nowhere but that a parser could still read differently from the check.
const AMBIGUOUS: &[(&str, &str)] = &[
    ("trailing .", "v1/board/."),
    ("trailing %2e", "v1/board/%2e"),
    ("NUL", "v1/bo%00ard"),
    ("empty middle segment", "v1//board"),
    ("literal percent", "v1/100%25"),
];

/// Every request target the host received.
#[derive(Clone, Default)]
struct HostLog(Arc<Mutex<Vec<String>>>);

impl HostLog {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

async fn recording_host() -> (String, HostLog) {
    let log = HostLog::default();
    let app = Router::new()
        .fallback(
            |State(log): State<HostLog>, req: Request<Body>| async move {
                let target = req
                    .uri()
                    .path_and_query()
                    .map(|p| p.as_str().to_string())
                    .unwrap_or_default();
                log.0.lock().unwrap().push(target);
                axum::Json(json!({"ok": true}))
            },
        )
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), log)
}

fn cfg(db_url: &str, host_url: &str) -> Config {
    Config {
        env: Env::Prod,
        bind_addr: "127.0.0.1:0".into(),
        database_url: db_url.into(),
        clerk_jwks_url: "https://clerk.test/jwks".into(),
        clerk_issuer: "https://clerk.test".into(),
        clerk_azp: vec![],
        dev_secret: None,
        auth_mode: AuthMode::Local,
        session_secret: Secret::new("session-secret-that-is-at-least-32-chars"),
        signup: SignupPolicy::Open,
        master_key: [9u8; 32],
        host_url: host_url.into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "https://api.wheel.test".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 10_000,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
        external: None,
        ws_max_bridges_per_project: 16,
        ws_max_lifetime_secs: 3600,
    }
}

struct Harness {
    app: Router,
    log: HostLog,
    alice: String,
    /// Alice's project, with ingress open.
    mine: String,
    /// Bob's project, with ingress open.
    theirs: String,
}

async fn call(app: &Router, method: &str, uri: &str, token: Option<&str>) -> (StatusCode, String) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("x-auth-token", t);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).to_string())
}

async fn call_json(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: serde_json::Value,
) -> serde_json::Value {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(t) = token {
        req = req.header("x-auth-token", t);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    assert!(
        res.status().is_success(),
        "{method} {uri}: {}",
        res.status()
    );
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn signup(app: &Router) -> String {
    let v = call_json(
        app,
        "POST",
        "/v1/auth/signup",
        None,
        json!({"email": format!("{}@example.test", uuid::Uuid::new_v4()),
               "password": "Correct-Horse-9!"}),
    )
    .await;
    v["token"].as_str().unwrap().to_string()
}

async fn project_with_ingress(app: &Router, token: &str) -> String {
    let v = call_json(
        app,
        "POST",
        "/v1/projects",
        Some(token),
        json!({"name": "p"}),
    )
    .await;
    let id = v["id"].as_str().unwrap().to_string();
    call_json(
        app,
        "PATCH",
        &format!("/v1/projects/{id}"),
        Some(token),
        json!({"capabilities": {"http": true}}),
    )
    .await;
    id
}

async fn harness() -> Harness {
    let (host, log) = recording_host().await;
    let path = std::env::temp_dir().join(format!("wheel-traversal-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let db = Db::connect(&url).await.expect("connect and migrate");
    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "https://clerk.test/jwks".into(),
            reqwest::Client::new(),
        ),
        cfg: cfg(&url, &host),
        db,
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(10_000),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
        // The real host URL layout, not a direct engine: that layout is what the bug escaped.
        engine_base_override: None,
        external_jwks: None,
        membership: wheel_api::membership::MembershipEvents::new(),
        bridges: wheel_api::http::bridges::BridgeCounter::new(),
    });
    let app = wheel_api::build_router(state, &[]);

    let alice = signup(&app).await;
    let bob = signup(&app).await;
    let mine = project_with_ingress(&app, &alice).await;
    let theirs = project_with_ingress(&app, &bob).await;
    log.take();
    Harness {
        app,
        log,
        alice,
        mine,
        theirs,
    }
}

/// Send each suffix beneath `prefix`. Returns a line for every one that was not refused or that
/// reached the host, and every request target the host saw.
async fn attempt(
    h: &Harness,
    prefix: &str,
    attacks: &[(&'static str, String)],
    token: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let mut escaped = Vec::new();
    let mut seen = Vec::new();
    for (label, suffix) in attacks {
        let (status, body) = call(&h.app, "GET", &format!("{prefix}/{suffix}"), token).await;
        let reached = h.log.take();
        if status != StatusCode::BAD_REQUEST || !reached.is_empty() {
            escaped.push(format!("{label}: {status} {body}, host saw {reached:?}"));
        }
        seen.extend(reached);
    }
    (escaped, seen)
}

fn assert_theirs_untouched(seen: &[String], theirs: &str, escaped: &[String]) {
    let prefix = format!("/host/v1/projects/{theirs}/");
    let crossed: Vec<&String> = seen.iter().filter(|t| t.starts_with(&prefix)).collect();
    assert!(
        crossed.is_empty(),
        "a request for project A reached project B on the host: {crossed:?}\nescapes:\n{}",
        escaped.join("\n")
    );
}

#[tokio::test]
async fn the_engine_proxy_never_carries_an_owner_into_another_project() {
    let h = harness().await;
    let (escaped, seen) = attempt(
        &h,
        &format!("/v1/projects/{}/engine", h.mine),
        &spellings(2, &[&h.theirs, "engine", "v1", "board"]),
        Some(&h.alice),
    )
    .await;
    assert_theirs_untouched(&seen, &h.theirs, &escaped);
    assert!(
        escaped.is_empty(),
        "not refused before the host:\n{}",
        escaped.join("\n")
    );
}

/// No credential at all: the ingress route is public, which is what made this critical.
#[tokio::test]
async fn public_ingress_never_carries_an_anonymous_caller_into_another_project() {
    let h = harness().await;
    let (escaped, seen) = attempt(
        &h,
        &format!("/p/{}", h.mine),
        &spellings(2, &[&h.theirs, "engine", "v1", "board"]),
        None,
    )
    .await;
    assert_theirs_untouched(&seen, &h.theirs, &escaped);
    assert!(
        escaped.is_empty(),
        "not refused before the host:\n{}",
        escaped.join("\n")
    );
}

/// One step up from a project's ingress base is the same project's engine route on the host,
/// which the host serves with the engine secret.
#[tokio::test]
async fn public_ingress_cannot_climb_into_its_own_projects_control_plane() {
    let h = harness().await;
    let (escaped, seen) = attempt(
        &h,
        &format!("/p/{}", h.mine),
        &spellings(1, &["engine", "v1", "board"]),
        None,
    )
    .await;
    let mount = format!("/host/v1/projects/{}/ingress/", h.mine);
    let outside: Vec<&String> = seen.iter().filter(|t| !t.starts_with(&mount)).collect();
    assert!(
        outside.is_empty(),
        "an ingress hit left the ingress mount: {outside:?}"
    );
    assert!(
        escaped.is_empty(),
        "not refused before the host:\n{}",
        escaped.join("\n")
    );
}

#[tokio::test]
async fn ambiguous_segments_are_refused_without_echoing_the_path() {
    let h = harness().await;
    for (prefix, token) in [
        (
            format!("/v1/projects/{}/engine", h.mine),
            Some(h.alice.as_str()),
        ),
        (format!("/p/{}", h.mine), None),
    ] {
        for (label, suffix) in AMBIGUOUS {
            let (status, body) = call(&h.app, "GET", &format!("{prefix}/{suffix}"), token).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{prefix} {label}: {body}");
            assert!(!body.contains(suffix), "{prefix} {label} echoed: {body}");
            let reached = h.log.take();
            assert!(reached.is_empty(), "{prefix} {label} reached {reached:?}");
        }
    }
}

#[tokio::test]
async fn ordinary_engine_paths_and_queries_reach_the_callers_own_project() {
    let h = harness().await;
    let mine = &h.mine;
    for (suffix, target) in [
        (
            "v1/board",
            format!("/host/v1/projects/{mine}/engine/v1/board"),
        ),
        (
            "v1/nodes?dry_run=1&tag=a%2Fb",
            format!("/host/v1/projects/{mine}/engine/v1/nodes?dry_run=1&tag=a%2Fb"),
        ),
        (
            "v1/vault/some%20key",
            format!("/host/v1/projects/{mine}/engine/v1/vault/some%20key"),
        ),
        (
            "v1/board/",
            format!("/host/v1/projects/{mine}/engine/v1/board/"),
        ),
    ] {
        let uri = format!("/v1/projects/{mine}/engine/{suffix}");
        let (status, body) = call(&h.app, "GET", &uri, Some(&h.alice)).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        assert_eq!(h.log.take(), vec![target], "{uri}");
    }
}

#[tokio::test]
async fn ordinary_ingress_hits_and_queries_reach_the_callers_own_project() {
    let h = harness().await;
    let mine = &h.mine;
    for (suffix, target) in [
        ("hook", format!("/host/v1/projects/{mine}/ingress/hook")),
        (
            "hook/abc?x=1&y=%2e%2e",
            format!("/host/v1/projects/{mine}/ingress/hook/abc?x=1&y=%2e%2e"),
        ),
        (
            "tg/a%20b",
            format!("/host/v1/projects/{mine}/ingress/tg/a%20b"),
        ),
    ] {
        let uri = format!("/p/{mine}/{suffix}");
        let (status, body) = call(&h.app, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        assert_eq!(h.log.take(), vec![target], "{uri}");
    }
}

/// The upgrade branch builds its own URL for the bridge, so it gets its own proof.
#[tokio::test]
async fn a_websocket_upgrade_on_a_traversing_path_is_refused_before_the_bridge() {
    use tokio_tungstenite::tungstenite::handshake::client::generate_key;

    let h = harness().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = h.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let url = format!(
        "ws://{addr}/v1/projects/{}/engine/%252e%252e/%252e%252e/{}/engine/v1/events",
        h.mine, h.theirs
    );
    let req = tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(&url)
        .header("x-auth-token", &h.alice)
        .header("Host", addr.to_string())
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key())
        .body(())
        .unwrap();
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("a traversing upgrade must not open");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST)
        }
        other => panic!("expected an HTTP refusal, got {other:?}"),
    }
    let seen = h.log.take();
    assert!(seen.is_empty(), "the host was dialled: {seen:?}");
}

/// The `%` refusal in `wheel_core::proxy_path` is sound only because axum decodes a wildcard
/// exactly once: a second decode would hand the proxy something the check never saw. This pins
/// the extractor shape the proxies use, so an axum upgrade that changes it goes red here.
#[tokio::test]
async fn axum_decodes_the_wildcard_exactly_once() {
    let app = Router::new().route(
        "/{id}/{*rest}",
        axum::routing::any(
            |axum::extract::Path((_, rest)): axum::extract::Path<(uuid::Uuid, String)>| async move {
                rest
            },
        ),
    );
    let id = uuid::Uuid::new_v4();
    for (raw, decoded) in [
        ("%2525", "%25"),
        ("%252e%252e/x", "%2e%2e/x"),
        ("%2e%2e/x", "../x"),
        ("a%2fb", "a/b"),
        ("a%5cb", "a\\b"),
        ("a%20b", "a b"),
    ] {
        let (status, body) = call(&app, "GET", &format!("/{id}/{raw}"), None).await;
        assert_eq!(status, StatusCode::OK, "{raw}");
        assert_eq!(body, decoded, "{raw}");
    }
}
