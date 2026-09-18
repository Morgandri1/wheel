// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! What bounds an **established** WebSocket bridge — ADVERSARY 011.
//!
//! 011 is explicit that the handshake timeout bounds only the handshake: once a bridge exists it has
//! no idle timeout, no lifetime cap, and no per-project connection cap, and on the production
//! `process` backend one project holding many silent-but-established bridges is a *cross-tenant*
//! availability problem rather than a self-inflicted one.
//!
//! The controls are only worth claiming if they can be watched working, so these are real sockets
//! against the real router rather than unit tests of the counter. `http::bridges` has the unit
//! tests; this is the half that proves the counter is actually consulted, that the actor markers
//! reach the engine on a path that forwards no client headers at all, and that a revoked member's
//! socket is closed rather than left open until it happens to end.
//!
//! On **SQLite**, deliberately: `ws_bridge_db.rs` covers the same bridge on Postgres but skips
//! without `TEST_DATABASE_URL`, so it does not run on a laptop — and a control that only runs where
//! somebody remembered to set an environment variable is one nobody watches go red.

// Exercises the SQLite backend, so it exists only in a build that has one.
#![cfg(feature = "sqlite")]

use axum::extract::State;
use axum::http::HeaderMap;
use futures_util::StreamExt;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wheel_api::config::{AuthMode, Config, Env, SignupPolicy};
use wheel_api::crypto::Secret;
use wheel_api::db::Db;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

/// Headers the engine saw on each accepted upgrade. The WS path builds a *fresh* request and
/// forwards no client header at all, so anything here got there because the API put it there.
#[derive(Clone, Default)]
struct UpgradeLog(Arc<Mutex<Vec<HeaderMap>>>);

/// An "engine" that accepts a websocket, records the handshake headers, and then stays silent —
/// which is exactly the shape of a legitimately idle events stream.
async fn mock_engine_ws() -> (String, UpgradeLog) {
    let log = UpgradeLog::default();
    let app = axum::Router::new()
        .route(
            "/v1/events",
            axum::routing::get(
                |State(log): State<UpgradeLog>,
                 headers: HeaderMap,
                 ws: axum::extract::ws::WebSocketUpgrade| async move {
                    log.0.lock().unwrap().push(headers);
                    ws.on_upgrade(|mut socket| async move {
                        // Echo, so a test can prove the bridge still relays while bounded.
                        while let Some(Ok(msg)) = socket.recv().await {
                            if socket.send(msg).await.is_err() {
                                break;
                            }
                        }
                    })
                },
            ),
        )
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), log)
}

fn cfg(db_url: &str, engine: &str, max_bridges: usize, lifetime: u64) -> Config {
    let _ = engine;
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
        master_key: [8u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "https://api.wheel.test".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 10_000,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
        ws_max_bridges_per_project: max_bridges,
        ws_max_lifetime_secs: lifetime,
    }
}

struct Api {
    base: String,
    db: Db,
    membership: wheel_api::membership::MembershipEvents,
    engine: UpgradeLog,
}

async fn serve(max_bridges: usize, lifetime: u64) -> Api {
    let (engine_base, engine) = mock_engine_ws().await;
    let path = std::env::temp_dir().join(format!("wheel-wsb-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let db = Db::connect(&url).await.expect("connect and migrate");
    let membership = wheel_api::membership::MembershipEvents::new();

    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "https://clerk.test/jwks".into(),
            reqwest::Client::new(),
        ),
        cfg: cfg(&url, &engine_base, max_bridges, lifetime),
        db: db.clone(),
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(10_000),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(10_000, 10_000),
        engine_base_override: Some(engine_base),
        membership: membership.clone(),
        bridges: wheel_api::http::bridges::BridgeCounter::new(),
    });

    let app = wheel_api::build_router(state, &[]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Api {
        base: format!("http://{addr}"),
        db,
        membership,
        engine,
    }
}

async fn post(
    base: &str,
    path: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> (reqwest::StatusCode, serde_json::Value) {
    let c = reqwest::Client::new();
    let mut req = c.post(format!("{base}{path}"));
    if let Some(t) = token {
        req = req.header("x-auth-token", t);
    }
    if let Some(b) = body {
        req = req.json(&b);
    }
    let res = req.send().await.unwrap();
    let status = res.status();
    (status, res.json().await.unwrap_or(serde_json::Value::Null))
}

async fn signup(base: &str, email: &str) -> (String, String) {
    let (status, body) = post(
        base,
        "/v1/auth/signup",
        None,
        Some(json!({"email": email, "password": "Correct-Horse-9!"})),
    )
    .await;
    assert_eq!(status, 201, "signup failed: {body}");
    let token = body["token"].as_str().unwrap().to_string();
    let c = reqwest::Client::new();
    let me: serde_json::Value = c
        .get(format!("{base}/v1/auth/me"))
        .header("x-auth-token", &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    (token, me["id"].as_str().unwrap().to_string())
}

async fn project(base: &str, token: &str) -> String {
    let (status, body) = post(
        base,
        "/v1/projects",
        Some(token),
        Some(json!({"name": "ws"})),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    body["id"].as_str().unwrap().to_string()
}

async fn ticket(base: &str, token: &str, pid: &str) -> String {
    let (status, body) = post(
        base,
        &format!("/v1/projects/{pid}/ws-ticket"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    body["ticket"].as_str().unwrap().to_string()
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn open(base: &str, token: &str, pid: &str) -> Result<Socket, String> {
    let t = ticket(base, token, pid).await;
    let url = format!(
        "{}/v1/projects/{pid}/engine/v1/events?ticket={t}",
        base.replace("http://", "ws://")
    );
    match tokio_tungstenite::connect_async(&url).await {
        Ok((s, _)) => Ok(s),
        Err(e) => Err(e.to_string()),
    }
}

/// **The cross-tenant control.** 011 calls the per-project cap the highest-priority of its three
/// recommendations, because it bounds the blast radius whatever the idle story turns out to be.
#[tokio::test]
async fn a_project_cannot_hold_more_bridges_than_its_cap() {
    let api = serve(2, 3600).await;
    let (token, _) = signup(&api.base, "cap@example.com").await;
    let pid = project(&api.base, &token).await;

    let _a = open(&api.base, &token, &pid).await.expect("first bridge");
    let _b = open(&api.base, &token, &pid).await.expect("second bridge");

    let refused = open(&api.base, &token, &pid)
        .await
        .expect_err("the third bridge must be refused");
    assert!(
        refused.contains("503"),
        "the cap should answer 503 — a concurrency ceiling, not a rate and not a bad gateway: {refused}"
    );

    // A closed bridge frees its slot: the counter releases on drop, so every way a socket can end
    // returns the slot rather than only the tidy one.
    drop(_a);
    // Give the server side a moment to observe the close.
    for _ in 0..50 {
        if open(&api.base, &token, &pid).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("a slot was never released after a bridge closed");
}

/// One project's bridges must not consume another's budget — otherwise the cap is a global limit
/// wearing a per-project label, and one tenant starves the rest.
#[tokio::test]
async fn one_projects_bridges_do_not_consume_anothers_budget() {
    let api = serve(1, 3600).await;
    let (token, _) = signup(&api.base, "two@example.com").await;
    let a = project(&api.base, &token).await;
    let b = project(&api.base, &token).await;

    let _first = open(&api.base, &token, &a).await.expect("project a");
    assert!(
        open(&api.base, &token, &a).await.is_err(),
        "a is at its cap"
    );
    let _other = open(&api.base, &token, &b)
        .await
        .expect("project b has its own budget");
}

/// The WS path builds a fresh request and forwards no client header, so the actor markers exist
/// upstream only because this path adds them explicitly. Assuming the HTTP path's behaviour covers
/// it is the mistake available at that line.
#[tokio::test]
async fn the_websocket_handshake_carries_the_actor_markers() {
    let api = serve(4, 3600).await;
    let (token, user_id) = signup(&api.base, "actor@example.com").await;
    let pid = project(&api.base, &token).await;
    let _s = open(&api.base, &token, &pid).await.expect("a bridge");

    let seen = api.engine.0.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "the engine saw {} upgrades", seen.len());
    let h = &seen[0];
    assert_eq!(
        h.get("x-wheel-actor-id").unwrap().to_str().unwrap(),
        user_id,
        "the upgrade reached the engine unattributed"
    );
    assert_eq!(h.get("x-wheel-actor-tier").unwrap(), "admin");
    // The ticket branch produces its own credential name, so the engine can tell how the socket was
    // opened — a ticket is not a session.
    assert_eq!(h.get("x-wheel-actor-credential").unwrap(), "ws_ticket");
    // The ticket has been consumed and must not travel further down the chain.
    assert!(h.get("x-auth-token").is_none());
}

/// A revoked member's socket closes, rather than surviving until it happens to end. On SQLite the
/// notification is in-process; the periodic re-check is what makes this certain on either backend,
/// and this proves the fast path is wired at all.
#[tokio::test]
async fn revoking_a_member_closes_their_live_bridge() {
    let api = serve(4, 3600).await;
    let (owner, _) = signup(&api.base, "owner@example.com").await;
    let (guest, guest_id) = signup(&api.base, "guest@example.com").await;
    let pid = project(&api.base, &owner).await;

    let (status, body) = post(
        &api.base,
        &format!("/v1/projects/{pid}/members"),
        Some(&owner),
        Some(json!({"user_id": guest_id, "role": "guest"})),
    )
    .await;
    assert_eq!(status, 201, "{body}");

    let mut socket = open(&api.base, &guest, &pid)
        .await
        .expect("the guest's bridge");

    let c = reqwest::Client::new();
    let res = c
        .delete(format!("{}/v1/projects/{pid}/members/{guest_id}", api.base))
        .header("x-auth-token", &owner)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);

    // The socket must end. Reading returns `None` (clean close) or an error; either is closure, and
    // asserting on *which* would be asserting on tungstenite rather than on us.
    let closed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match socket.next().await {
                None => return true,
                Some(Err(_)) => return true,
                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => return true,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert!(
        matches!(closed, Ok(true)),
        "a revoked member's bridge stayed open"
    );
}

/// The absolute cap. Set to one second here; in production it is an hour, and it is the thing that
/// bounds how long a missed revocation can persist even if the notification and the re-check both
/// fail.
#[tokio::test]
async fn a_bridge_does_not_outlive_its_lifetime_cap() {
    let api = serve(4, 1).await;
    let (token, _) = signup(&api.base, "life@example.com").await;
    let pid = project(&api.base, &token).await;
    let mut socket = open(&api.base, &token, &pid).await.expect("a bridge");

    let closed = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match socket.next().await {
                None | Some(Err(_)) => return true,
                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => return true,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert!(matches!(closed, Ok(true)), "a bridge outlived its cap");

    // And the slot came back, so a client that reconnects after the cap is not locked out.
    let _ = &api.db;
    let _ = &api.membership;
    assert!(
        open(&api.base, &token, &pid).await.is_ok(),
        "the capped bridge did not release its slot"
    );
}
