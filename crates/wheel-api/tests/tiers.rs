// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! What a lower tier can attempt, and be refused.
//!
//! `docs/proposals/shared-projects.md` §5 is the specification — every route in exactly one of
//! admin, prompter, guest, and anything unlisted refused. This file is what makes that specification
//! *mechanical*: a handler that forgets its `require` call, or an engine path that nobody wrote a
//! policy row for, fails here rather than shipping.
//!
//! Two axes, and the second is the one that would be missed by testing only the obvious path:
//!
//!   * **honest** — the caller's real tier is lower than the route needs.
//!   * **forged** — the caller supplies `x-wheel-actor-*` headers claiming a tier they do not hold.
//!     This is the hole at `routes/proxy.rs` where `sanitize_for_upstream` was given an empty
//!     prefix list — `redteam/findings/052-authenticated-proxy-does-not-strip-x-wheel-namespace.md`
//!     — so it is tested as its own axis rather than assumed covered by the honest one.
//!
//! Runs on SQLite so it needs no `TEST_DATABASE_URL` and runs everywhere.

// Exercises the SQLite backend, so it exists only in a build that has one.
#![cfg(feature = "sqlite")]

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uuid::Uuid;
use wheel_api::config::{AuthMode, Config, Env, SignupPolicy};
use wheel_api::crypto::Secret;
use wheel_api::db::Db;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

/// Everything the engine actually received: the request target and the headers that survived the
/// API's hop. Both matter — the target proves the path was forwarded, the headers prove what the
/// API asserted about the caller.
#[derive(Clone, Default)]
struct EngineLog(Arc<Mutex<Vec<(String, HeaderMap)>>>);

impl EngineLog {
    fn take(&self) -> Vec<(String, HeaderMap)> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }

    /// The single request the engine saw, or a panic naming how many it saw instead.
    fn only(&self) -> (String, HeaderMap) {
        let mut all = self.take();
        assert_eq!(all.len(), 1, "expected exactly one upstream request");
        all.pop().unwrap()
    }
}

async fn recording_engine() -> (String, EngineLog) {
    let log = EngineLog::default();
    let app = Router::new()
        .fallback(
            |State(log): State<EngineLog>, req: Request<Body>| async move {
                let target = req
                    .uri()
                    .path_and_query()
                    .map(|p| p.as_str().to_string())
                    .unwrap_or_default();
                log.0.lock().unwrap().push((target, req.headers().clone()));
                axum::Json(json!({"ok": true}))
            },
        )
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), log)
}

fn cfg(db_url: &str) -> Config {
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
        master_key: [3u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "https://api.wheel.test".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 10_000,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
        ws_max_bridges_per_project: 16,
        ws_max_lifetime_secs: 3600,
    }
}

struct Harness {
    app: Router,
    db: Db,
    engine: EngineLog,
    /// Created the project, so admin of it.
    creator: String,
    creator_id: String,
    prompter: String,
    prompter_id: String,
    guest: String,
    guest_id: String,
    /// A member of nothing.
    outsider: String,
    outsider_id: String,
    project: String,
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    call_with(app, method, uri, token, body, &[]).await
}

/// The same, plus arbitrary extra request headers — how a forged `x-wheel-*` is presented.
async fn call_with(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
    extra: &[(&str, &str)],
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("x-auth-token", t);
    }
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn signup(app: &Router, email: &str) -> (String, String) {
    let (status, body) = call(
        app,
        "POST",
        "/v1/auth/signup",
        None,
        Some(json!({"email": email, "password": "Correct-Horse-9!"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "signup failed: {body}");
    let token = body["token"].as_str().expect("a token").to_string();
    let (_, me) = call(app, "GET", "/v1/auth/me", Some(&token), None).await;
    let id = me["id"].as_str().expect("an id").to_string();
    (token, id)
}

async fn harness() -> Harness {
    let path = std::env::temp_dir().join(format!("wheel-tiers-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let db = Db::connect(&url).await.expect("connect and migrate");
    let (engine_base, engine) = recording_engine().await;

    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "https://clerk.test/jwks".into(),
            reqwest::Client::new(),
        ),
        cfg: cfg(&url),
        db: db.clone(),
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(10_000),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(10_000, 10_000),
        engine_base_override: Some(engine_base),
        membership: wheel_api::membership::MembershipEvents::new(),
        bridges: wheel_api::http::bridges::BridgeCounter::new(),
    });
    let app = wheel_api::build_router(state, &[]);

    let (creator, creator_id) = signup(&app, "creator@example.com").await;
    let (prompter, prompter_id) = signup(&app, "prompter@example.com").await;
    let (guest, guest_id) = signup(&app, "guest@example.com").await;
    let (outsider, outsider_id) = signup(&app, "outsider@example.com").await;

    let (status, project) = call(
        &app,
        "POST",
        "/v1/projects",
        Some(&creator),
        Some(json!({"name": "shared"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create failed: {project}");
    let project = project["id"].as_str().expect("a project id").to_string();

    for (id, role) in [(&prompter_id, "prompter"), (&guest_id, "guest")] {
        let (status, body) = call(
            &app,
            "POST",
            &format!("/v1/projects/{project}/members"),
            Some(&creator),
            Some(json!({"user_id": id, "role": role})),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "granting {role} failed: {body}"
        );
    }

    engine.take();
    Harness {
        app,
        db,
        engine,
        creator,
        creator_id,
        prompter,
        prompter_id,
        guest,
        guest_id,
        outsider,
        outsider_id,
        project,
    }
}

const AGENT: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";

/// Engine paths, by the lowest tier that may reach them. The table under test is
/// `auth::policy::RULES`; this is the same information expressed as requests, so a rule that is
/// written but not reachable — or reachable but not written — shows up as a failure here.
fn engine_paths() -> Vec<(&'static str, String, &'static str)> {
    vec![
        ("GET", "v1/board".into(), "guest"),
        ("GET", "v1/engine".into(), "guest"),
        ("GET", format!("v1/agents/{AGENT}/log"), "guest"),
        ("GET", format!("v1/agents/{AGENT}/inbox"), "guest"),
        ("GET", format!("v1/tables/{AGENT}/rows"), "guest"),
        ("GET", format!("v1/tools/{AGENT}/ops"), "guest"),
        ("GET", format!("v1/agents/{AGENT}/auth"), "guest"),
        ("POST", format!("v1/agents/{AGENT}/send"), "prompter"),
        ("POST", format!("v1/agents/{AGENT}/start"), "prompter"),
        ("POST", format!("v1/agents/{AGENT}/stop"), "prompter"),
        ("POST", format!("v1/agents/{AGENT}/restart"), "prompter"),
        ("POST", format!("v1/agents/{AGENT}/clear"), "prompter"),
        ("PUT", format!("v1/nodes/{AGENT}/content"), "prompter"),
        ("POST", format!("v1/tables/{AGENT}/query"), "prompter"),
        ("POST", "v1/nodes".into(), "admin"),
        ("PATCH", format!("v1/nodes/{AGENT}"), "admin"),
        ("DELETE", format!("v1/nodes/{AGENT}"), "admin"),
        ("POST", "v1/wires".into(), "admin"),
        ("DELETE", "v1/wires".into(), "admin"),
        ("GET", format!("v1/vault/{AGENT}"), "admin"),
        ("PUT", format!("v1/vault/{AGENT}/KEY"), "admin"),
        ("DELETE", format!("v1/vault/{AGENT}/KEY"), "admin"),
        ("DELETE", format!("v1/agents/{AGENT}/auth"), "admin"),
        ("POST", format!("v1/agents/{AGENT}/auth/begin"), "admin"),
        ("POST", format!("v1/agents/{AGENT}/auth/complete"), "admin"),
        ("POST", format!("v1/tools/{AGENT}/call"), "admin"),
        ("POST", "v1/tools/import".into(), "admin"),
        ("POST", format!("v1/tools/{AGENT}/import"), "admin"),
    ]
}

fn rank(tier: &str) -> u8 {
    match tier {
        "guest" => 0,
        "prompter" => 1,
        _ => 2,
    }
}

// --------------------------------------------------------------------------- honest refusals

/// The core matrix: for every engine path, every tier below it is refused and the request never
/// reaches the engine at all.
#[tokio::test]
async fn a_lower_tier_is_refused_every_engine_path_above_it() {
    let h = harness().await;
    let actors = [
        ("guest", &h.guest),
        ("prompter", &h.prompter),
        ("admin", &h.creator),
    ];

    for (method, path, needed) in engine_paths() {
        for (tier, token) in actors {
            let uri = format!("/v1/projects/{}/engine/{path}", h.project);
            let (status, _) = call(&h.app, method, &uri, Some(token), Some(json!({}))).await;

            if rank(tier) < rank(needed) {
                assert_eq!(
                    status,
                    StatusCode::FORBIDDEN,
                    "a {tier} reached {method} {path}, which needs {needed}"
                );
                assert!(
                    h.engine.take().is_empty(),
                    "a refused {method} {path} still reached the engine as {tier}"
                );
            } else {
                assert_ne!(
                    status,
                    StatusCode::FORBIDDEN,
                    "a {tier} was refused {method} {path}, which only needs {needed}"
                );
                assert_eq!(
                    h.engine.take().len(),
                    1,
                    "an allowed {method} {path} did not reach the engine as {tier}"
                );
            }
        }
    }
}

/// The node-token realm is refused to everyone, admins included. It is the agent-token plane, and
/// the API cannot attribute an actor there — so it refuses to carry one.
#[tokio::test]
async fn the_cli_realm_is_refused_to_every_tier_including_admin() {
    let h = harness().await;
    for token in [&h.guest, &h.prompter, &h.creator] {
        for path in [
            "v1/cli/whoami",
            "v1/cli/msg",
            "v1/cli/secret",
            "v1/cli/query",
        ] {
            let uri = format!("/v1/projects/{}/engine/{path}", h.project);
            let (status, _) = call(&h.app, "POST", &uri, Some(token), Some(json!({}))).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path} was reachable");
            assert!(h.engine.take().is_empty(), "{path} reached the engine");
        }
    }
}

/// Default DENY: an engine path with no policy row is unreachable, even for an admin. Adding a
/// route to the engine without adding a rule must make it invisible, not public.
#[tokio::test]
async fn an_engine_path_with_no_rule_is_refused_even_for_an_admin() {
    let h = harness().await;
    for path in ["v1/unknown", "v2/board", "v1", "v1/board/extra"] {
        let uri = format!("/v1/projects/{}/engine/{path}", h.project);
        let (status, _) = call(&h.app, "GET", &uri, Some(&h.creator), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path} was reachable");
        assert!(h.engine.take().is_empty(), "{path} reached the engine");
    }
    // A method nobody granted on a path somebody did.
    let uri = format!("/v1/projects/{}/engine/v1/board", h.project);
    let (status, _) = call(&h.app, "DELETE", &uri, Some(&h.creator), None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "DELETE /v1/board was reachable"
    );
}

/// API routes, as opposed to proxied engine paths. Each handler carries its own `require`, and this
/// is what proves none of them forgot.
#[tokio::test]
async fn a_lower_tier_is_refused_every_admin_api_route() {
    let h = harness().await;
    let p = &h.project;
    let admin_only: Vec<(&str, String, Option<serde_json::Value>)> = vec![
        (
            "PATCH",
            format!("/v1/projects/{p}"),
            Some(json!({"name": "renamed"})),
        ),
        ("POST", format!("/v1/projects/{p}/start"), None),
        ("POST", format!("/v1/projects/{p}/stop"), None),
        ("POST", format!("/v1/projects/{p}/restart"), None),
        (
            "POST",
            format!("/v1/projects/{p}/board/apply"),
            Some(json!({"board": {"nodes": [], "wires": []}})),
        ),
        (
            "POST",
            format!("/v1/projects/{p}/members"),
            Some(json!({"user_id": "x", "role": "guest"})),
        ),
        ("DELETE", format!("/v1/projects/{p}/members/x"), None),
        ("GET", format!("/v1/projects/{p}/invites"), None),
        (
            "POST",
            format!("/v1/projects/{p}/invites"),
            Some(json!({"role": "guest"})),
        ),
        ("DELETE", format!("/v1/projects/{p}"), None),
    ];

    for (method, uri, body) in admin_only {
        for (tier, token) in [("guest", &h.guest), ("prompter", &h.prompter)] {
            let (status, _) = call(&h.app, method, &uri, Some(token), body.clone()).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "a {tier} reached {method} {uri}"
            );
        }
    }
}

/// Reading the project and taking a ws-ticket are guest capabilities: a guest who cannot read is
/// not a tier, it is an exclusion.
#[tokio::test]
async fn a_guest_may_read_the_project_and_take_a_ticket() {
    let h = harness().await;
    let (status, body) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{}", h.project),
        Some(&h.guest),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["tier"], "guest",
        "the response says what the caller may do"
    );

    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/{}/ws-ticket", h.project),
        Some(&h.guest),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// A non-member gets 404, never 403. 403 would confirm the project exists, which is the
/// enumeration oracle `load_member` keeps closed by making access a `WHERE` predicate.
#[tokio::test]
async fn a_non_member_is_told_nothing_at_all() {
    let h = harness().await;
    for (method, suffix) in [
        ("GET", ""),
        ("PATCH", ""),
        ("DELETE", ""),
        ("POST", "/stop"),
    ] {
        let (status, _) = call(
            &h.app,
            method,
            &format!("/v1/projects/{}{suffix}", h.project),
            Some(&h.outsider),
            Some(json!({"name": "x"})),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{method} leaked the project's existence to a non-member"
        );
    }
    let (status, _) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{}/engine/v1/board", h.project),
        Some(&h.outsider),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A revoked member is a non-member: 404 again, not 403, and the same on the next request rather
/// than only after something expires.
#[tokio::test]
async fn a_revoked_member_stops_being_a_member() {
    let h = harness().await;
    let uri = format!("/v1/projects/{}", h.project);

    let (status, _) = call(&h.app, "GET", &uri, Some(&h.prompter), None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = call(
        &h.app,
        "DELETE",
        &format!("/v1/projects/{}/members/{}", h.project, h.prompter_id),
        Some(&h.creator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = call(&h.app, "GET", &uri, Some(&h.prompter), None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a revoked member still had access"
    );
}

// --------------------------------------------------------------------------- the creator

/// The creator's admin comes from `projects.owner_id`, so there is nothing to demote and no row to
/// remove. Both attempts are refused rather than silently doing nothing, which is what would
/// happen if the two sources of truth were allowed to coexist.
#[tokio::test]
async fn the_creator_cannot_be_demoted_or_removed() {
    let h = harness().await;
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/{}/members", h.project),
        Some(&h.creator),
        Some(json!({"user_id": h.creator_id, "role": "guest"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "the creator was given a role");

    let (status, _) = call(
        &h.app,
        "DELETE",
        &format!("/v1/projects/{}/members/{}", h.project, h.creator_id),
        Some(&h.creator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "the creator was removed");

    // And they still are an admin afterwards.
    let (status, body) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{}", h.project),
        Some(&h.creator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tier"], "admin");
}

/// A second admin is possible — the ruling puts member management in that tier, which implies an
/// admin may make another — and they get the full admin surface, not a diminished one.
#[tokio::test]
async fn an_admin_who_is_not_the_creator_has_the_admin_surface() {
    let h = harness().await;
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/{}/members", h.project),
        Some(&h.creator),
        Some(json!({"user_id": h.prompter_id, "role": "admin"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // A rename is admin, and it must actually take effect rather than matching zero rows: the
    // statement is keyed on the project, not on `owner_id`, precisely so a non-creator admin works.
    let (status, body) = call(
        &h.app,
        "PATCH",
        &format!("/v1/projects/{}", h.project),
        Some(&h.prompter),
        Some(json!({"name": "renamed by the other admin"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "renamed by the other admin");
}

// --------------------------------------------------------------------------- forged headers

/// The forged axis. A caller who sets `x-wheel-actor-*` must not gain anything by it, and the
/// values that reach the engine must be the server's — *replacing* theirs, not sitting beside them.
#[tokio::test]
async fn a_forged_actor_header_never_reaches_the_engine() {
    let h = harness().await;
    let forged = [
        ("x-wheel-actor-id", "creator@example.com"),
        ("x-wheel-actor-tier", "admin"),
        ("x-wheel-actor-credential", "session"),
        ("x-wheel-ingress", "1"),
        ("x-wheel-client-ip", "203.0.113.9"),
        ("x-wheel-anything-else", "hello"),
    ];

    // On a route the guest IS allowed, so the request reaches the engine and we can read what
    // arrived. This is the case that proves replacement rather than mere absence.
    let uri = format!("/v1/projects/{}/engine/v1/board", h.project);
    let (status, _) = call_with(&h.app, "GET", &uri, Some(&h.guest), None, &forged).await;
    assert_eq!(status, StatusCode::OK);

    let (_, headers) = h.engine.only();
    assert_eq!(
        headers.get("x-wheel-actor-id").unwrap(),
        h.guest_id.as_str(),
        "the engine was told the forged actor, not the real one"
    );
    assert_eq!(headers.get("x-wheel-actor-tier").unwrap(), "guest");
    assert_eq!(headers.get("x-wheel-actor-credential").unwrap(), "session");
    // One value, not two: `HeaderMap` can hold both, and "ignored" would leave the forgery there.
    assert_eq!(headers.get_all("x-wheel-actor-tier").iter().count(), 1);
    assert_eq!(headers.get_all("x-wheel-actor-id").iter().count(), 1);
    // The rest of the namespace is stripped outright, including names we do not set ourselves.
    assert!(headers.get("x-wheel-ingress").is_none());
    assert!(headers.get("x-wheel-client-ip").is_none());
    assert!(headers.get("x-wheel-anything-else").is_none());
    // And the client's own credential never crosses the hop.
    assert!(headers.get("x-auth-token").is_none());
}

/// Forging a tier does not buy the route that tier would have opened.
#[tokio::test]
async fn a_forged_tier_does_not_open_a_route() {
    let h = harness().await;
    let forged = [
        ("x-wheel-actor-tier", "admin"),
        ("x-wheel-actor-id", "creator@example.com"),
    ];

    for (tier, token) in [("guest", &h.guest), ("prompter", &h.prompter)] {
        let uri = format!("/v1/projects/{}/engine/v1/vault/{AGENT}/KEY", h.project);
        let (status, _) =
            call_with(&h.app, "PUT", &uri, Some(token), Some(json!({})), &forged).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a {tier} forged their way into the vault"
        );
        assert!(h.engine.take().is_empty());

        // And on the API's own routes, where the tier comes from the same place.
        let uri = format!("/v1/projects/{}", h.project);
        let (status, _) = call_with(&h.app, "DELETE", &uri, Some(token), None, &forged).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a {tier} forged a project delete"
        );
    }
}

/// `board/apply` and `instantiate` reach the engine through `HttpBoardClient`, not through
/// `routes::proxy`. Policy and actor work applied only at the proxy would miss them entirely, so
/// they get their own assertion.
#[tokio::test]
async fn the_board_apply_path_carries_the_actor_and_refuses_lower_tiers() {
    let h = harness().await;
    let uri = format!("/v1/projects/{}/board/apply", h.project);
    let body = json!({"board": {"nodes": [], "wires": []}});

    for (tier, token) in [("guest", &h.guest), ("prompter", &h.prompter)] {
        let (status, _) = call(&h.app, "POST", &uri, Some(token), Some(body.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "a {tier} applied a board");
    }

    // As the creator it proceeds, and the engine read it did carries the actor.
    let (status, _) = call_with(
        &h.app,
        "POST",
        &uri,
        Some(&h.creator),
        Some(body),
        &[("x-wheel-actor-tier", "guest")],
    )
    .await;
    assert_ne!(status, StatusCode::FORBIDDEN);
    let seen = h.engine.take();
    assert!(!seen.is_empty(), "board/apply never reached the engine");
    for (target, headers) in seen {
        assert_eq!(
            headers.get("x-wheel-actor-id").map(|v| v.to_str().unwrap()),
            Some(h.creator_id.as_str()),
            "{target} arrived without the actor"
        );
        assert_eq!(
            headers.get("x-wheel-actor-tier").unwrap(),
            "admin",
            "{target} carried the forged tier"
        );
    }
}

// --------------------------------------------------------------------------- invites

#[tokio::test]
async fn an_invite_grants_the_tier_it_names() {
    let h = harness().await;
    let (status, invite) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/{}/invites", h.project),
        Some(&h.creator),
        Some(json!({"role": "prompter"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{invite}");
    let token = invite["token"]
        .as_str()
        .expect("an invite token")
        .to_string();
    assert!(token.starts_with("wi_"), "{token}");

    let (status, accepted) = call(
        &h.app,
        "POST",
        "/v1/invites/accept",
        Some(&h.outsider),
        Some(json!({"token": token})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["role"], "prompter");
    assert_eq!(accepted["project_id"], h.project);

    // And it is real access, not just a row.
    let (status, body) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{}", h.project),
        Some(&h.outsider),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tier"], "prompter");
}

/// A stale low-tier link must not become a way to demote someone. Accepting is idempotent and only
/// ever raises.
#[tokio::test]
async fn accepting_a_lower_invite_never_downgrades() {
    let h = harness().await;
    let (_, invite) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/{}/invites", h.project),
        Some(&h.creator),
        Some(json!({"role": "guest"})),
    )
    .await;
    let token = invite["token"].as_str().unwrap().to_string();

    let (status, accepted) = call(
        &h.app,
        "POST",
        "/v1/invites/accept",
        Some(&h.prompter),
        Some(json!({"token": token})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        accepted["role"], "prompter",
        "a guest link demoted a prompter"
    );
}

/// Single-use by default: the count is consumed inside the redeeming statement, so a second
/// attempt finds nothing rather than racing the first.
#[tokio::test]
async fn an_invite_is_single_use_by_default() {
    let h = harness().await;
    let (_, invite) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/{}/invites", h.project),
        Some(&h.creator),
        Some(json!({"role": "guest"})),
    )
    .await;
    let token = invite["token"].as_str().unwrap().to_string();

    let (first, _) = call(
        &h.app,
        "POST",
        "/v1/invites/accept",
        Some(&h.outsider),
        Some(json!({"token": token.clone()})),
    )
    .await;
    assert_eq!(first, StatusCode::OK);

    let (second, _) = call(
        &h.app,
        "POST",
        "/v1/invites/accept",
        Some(&h.outsider),
        Some(json!({"token": token})),
    )
    .await;
    assert_eq!(second, StatusCode::UNAUTHORIZED, "an invite was used twice");
}

/// A revoked invite is refused, and refused the same way an unknown one is — an invite link is a
/// credential, so the answer must not say which links exist.
#[tokio::test]
async fn a_revoked_invite_is_indistinguishable_from_an_unknown_one() {
    let h = harness().await;
    let (_, invite) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/{}/invites", h.project),
        Some(&h.creator),
        Some(json!({"role": "guest"})),
    )
    .await;
    let token = invite["token"].as_str().unwrap().to_string();
    let id = invite["id"].as_str().unwrap().to_string();

    let (status, _) = call(
        &h.app,
        "DELETE",
        &format!("/v1/projects/{}/invites/{id}", h.project),
        Some(&h.creator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (revoked, revoked_body) = call(
        &h.app,
        "POST",
        "/v1/invites/accept",
        Some(&h.outsider),
        Some(json!({"token": token})),
    )
    .await;
    let (unknown, unknown_body) = call(
        &h.app,
        "POST",
        "/v1/invites/accept",
        Some(&h.outsider),
        Some(json!({"token": "wi_nosuchtokenatallreallynone"})),
    )
    .await;
    assert_eq!(revoked, StatusCode::UNAUTHORIZED);
    assert_eq!(revoked, unknown);
    assert_eq!(revoked_body, unknown_body, "the two answers differ");
}

/// An email-locked invite checks the *account's verified* address, never a claim in the request.
#[tokio::test]
async fn an_email_locked_invite_only_opens_for_that_account() {
    let h = harness().await;
    let (status, invite) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/{}/invites", h.project),
        Some(&h.creator),
        Some(json!({"role": "guest", "email": "outsider@example.com", "max_uses": 5})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{invite}");
    let token = invite["token"].as_str().unwrap().to_string();

    // A different account cannot redeem it, whatever it says about itself.
    let (wrong, _) = call(
        &h.app,
        "POST",
        "/v1/invites/accept",
        Some(&h.guest),
        Some(json!({"token": token.clone(), "email": "outsider@example.com"})),
    )
    .await;
    assert_eq!(wrong, StatusCode::UNAUTHORIZED, "the lock was bypassed");

    let (right, _) = call(
        &h.app,
        "POST",
        "/v1/invites/accept",
        Some(&h.outsider),
        Some(json!({"token": token})),
    )
    .await;
    assert_eq!(right, StatusCode::OK);
}

/// Only an admin lists or creates invites — including *listing*, because an invite's existence and
/// tier are facts about who is about to gain access.
#[tokio::test]
async fn invites_are_invisible_below_admin() {
    let h = harness().await;
    for token in [&h.guest, &h.prompter] {
        let (status, _) = call(
            &h.app,
            "GET",
            &format!("/v1/projects/{}/invites", h.project),
            Some(token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}

// --------------------------------------------------------------------------- the project list

/// The list is the union of created and joined projects, and it agrees with `load_member` about
/// who can see what — a project missing from the list but reachable by id is the drift two
/// separate queries produce.
#[tokio::test]
async fn the_project_list_is_what_you_created_plus_what_you_joined() {
    let h = harness().await;

    let (status, mine) = call(&h.app, "GET", "/v1/projects", Some(&h.creator), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(mine.as_array().unwrap().len(), 1);
    assert_eq!(mine[0]["tier"], "admin");

    let (_, joined) = call(&h.app, "GET", "/v1/projects", Some(&h.guest), None).await;
    assert_eq!(
        joined.as_array().unwrap().len(),
        1,
        "a joined project is not listed"
    );
    assert_eq!(joined[0]["id"], h.project);
    assert_eq!(joined[0]["tier"], "guest");

    let (_, none) = call(&h.app, "GET", "/v1/projects", Some(&h.outsider), None).await;
    assert!(
        none.as_array().unwrap().is_empty(),
        "a non-member saw a project"
    );
}

/// A guest's project does not count against their own quota: charging them for a project somebody
/// else provisioned would let anyone exhaust anyone's quota by inviting them.
#[tokio::test]
async fn a_joined_project_does_not_consume_your_quota() {
    let h = harness().await;
    for i in 0..3 {
        let (status, _) = call(
            &h.app,
            "POST",
            "/v1/projects",
            Some(&h.guest),
            Some(json!({"name": format!("guest-own-{i}")})),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "guest could not create their own project"
        );
    }
    let (_, list) = call(&h.app, "GET", "/v1/projects", Some(&h.guest), None).await;
    assert_eq!(
        list.as_array().unwrap().len(),
        4,
        "three created plus one joined"
    );
}

/// Membership reading is a guest capability, and the creator is reported even though they are not a
/// row in `project_members`.
#[tokio::test]
async fn the_member_list_names_the_creator_who_is_not_a_row() {
    let h = harness().await;
    let (status, body) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{}/members", h.project),
        Some(&h.guest),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["creator"], h.creator_id);
    let members = body["members"].as_array().unwrap();
    assert_eq!(members.len(), 2, "prompter and guest, and not the creator");
    assert!(
        members.iter().all(|m| m["user_id"] != json!(h.creator_id)),
        "the creator appeared as a member row: {body}"
    );
}

// --------------------------------------------------------------------------- migration safety

/// **The test that stands between this change and locking the live deployment's owner out.**
///
/// `https://wheel.avo.so` holds projects created before `project_members` existed, so they have no
/// member rows and never will unless something writes them. Migration 0006 deliberately does not:
/// it creates two empty tables and touches no existing data, because the creator's admin is
/// *derived* from `projects.owner_id` rather than stored (`docs/proposals/shared-projects.md` §4.2).
///
/// So this inserts a project the way the live database already holds one — a bare row, no
/// membership, no invites — and proves the owner still has everything and a stranger still has
/// nothing. If the derivation were ever replaced by a lookup, or a backfill were introduced and got
/// it wrong, this is what would go red.
#[tokio::test]
async fn a_project_that_predates_membership_still_belongs_to_its_owner() {
    let h = harness().await;

    // Straight into the table, bypassing `POST /v1/projects` entirely: no member row is written,
    // which is exactly the state of every project that existed before this migration.
    let legacy = Uuid::new_v4();
    let caps = serde_json::to_value(wheel_api::models::Capabilities::default()).unwrap();
    wheel_api::db_execute!(
        &h.db,
        "INSERT INTO projects (id, owner_id, name, capabilities, status) \
         VALUES ($1, $2, $3, $4, 'stopped')",
        legacy,
        h.creator_id.as_str(),
        "a board from before sharing existed",
        &caps
    )
    .expect("seed a pre-membership project");

    // There is genuinely no membership row — so this is not passing for the wrong reason.
    let members: i64 = wheel_api::db_scalar!(
        &h.db,
        "SELECT count(*) FROM project_members WHERE project_id = $1",
        legacy
    )
    .expect("count members");
    assert_eq!(members, 0, "the fixture accidentally created a member row");

    // The owner is an admin of it, by derivation.
    let (status, body) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{legacy}"),
        Some(&h.creator),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the owner lost their own project: {body}"
    );
    assert_eq!(
        body["tier"], "admin",
        "the owner is not an admin of their own board"
    );

    // It is listed, so it does not merely exist — it is reachable the way a person finds it.
    let (_, list) = call(&h.app, "GET", "/v1/projects", Some(&h.creator), None).await;
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == json!(legacy.to_string())),
        "a pre-membership project vanished from its owner's project list: {list}"
    );

    // And admin-only actions work on it, not just the read.
    let (status, _) = call(
        &h.app,
        "PATCH",
        &format!("/v1/projects/{legacy}"),
        Some(&h.creator),
        Some(json!({"name": "renamed after the migration"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the owner could not administer their own project"
    );

    // Everyone else still gets nothing, and gets told nothing.
    for token in [&h.prompter, &h.guest, &h.outsider] {
        let (status, _) = call(
            &h.app,
            "GET",
            &format!("/v1/projects/{legacy}"),
            Some(token),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "a pre-membership project leaked to a non-member"
        );
    }
}

/// The other half of §0: an admin grants a tier by account id, and that account then *finds* the
/// project. Granting access that the grantee cannot discover is not sharing.
#[tokio::test]
async fn a_granted_account_sees_the_project_in_its_own_list() {
    let h = harness().await;

    // Before: the outsider has no projects at all.
    let (_, before) = call(&h.app, "GET", "/v1/projects", Some(&h.outsider), None).await;
    assert!(before.as_array().unwrap().is_empty());

    let (status, body) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/{}/members", h.project),
        Some(&h.creator),
        Some(json!({"user_id": h.outsider_id, "role": "guest"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (_, after) = call(&h.app, "GET", "/v1/projects", Some(&h.outsider), None).await;
    let listed = after.as_array().unwrap();
    assert_eq!(
        listed.len(),
        1,
        "the granted project is not in the grantee's list"
    );
    assert_eq!(listed[0]["id"], h.project);
    assert_eq!(listed[0]["tier"], "guest");

    // And it is real access: the board reads, and a send does not.
    let (status, _) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{}/engine/v1/board", h.project),
        Some(&h.outsider),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a granted guest could not read the board"
    );

    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/{}/engine/v1/agents/{AGENT}/send", h.project),
        Some(&h.outsider),
        Some(json!({"body": "hello"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a guest sent a message");
}

/// A role string this build cannot parse is **not access**.
///
/// The CHECK constraint on `project_members.role` makes this unreachable through the API today,
/// which is exactly why it needs a test that reaches it another way: the case it defends against is
/// a *rolling deploy*. Add a fourth tier in a later migration and, for the minutes during which both
/// versions are live, an old replica reads a row naming a tier it has never heard of. Rounding that
/// up is how a future `viewer` silently becomes an admin; rounding it down is the only direction
/// that cannot grant anything.
///
/// So the test recreates `project_members` without the CHECK — which is precisely what a future
/// schema version looks like to this binary — and writes the unknown tier directly.
#[tokio::test]
async fn a_role_this_build_cannot_parse_is_refused_rather_than_rounded() {
    let h = harness().await;

    // A future schema: same columns, no CHECK. Nothing else about the row changes.
    for stmt in [
        "DROP INDEX IF EXISTS project_members_user_idx",
        "ALTER TABLE project_members RENAME TO project_members_old",
        "CREATE TABLE project_members (
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            user_id    TEXT NOT NULL,
            role       TEXT NOT NULL,
            invited_by TEXT,
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
            updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
            revoked_at TEXT,
            PRIMARY KEY (project_id, user_id))",
        "INSERT INTO project_members SELECT * FROM project_members_old",
        "DROP TABLE project_members_old",
    ] {
        wheel_api::db_execute!(&h.db, stmt).expect("restage the table without its CHECK");
    }

    // The guest is promoted to a tier that does not exist in this build.
    let changed = wheel_api::db_execute!(
        &h.db,
        "UPDATE project_members SET role = 'superuser' WHERE project_id = $1 AND user_id = $2",
        Uuid::parse_str(&h.project).unwrap(),
        h.guest_id.as_str()
    )
    .expect("write the unknown tier");
    assert_eq!(
        changed, 1,
        "the fixture did not actually write an unknown role"
    );

    // It buys nothing — not even the read a guest had a moment ago.
    let (status, _) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{}", h.project),
        Some(&h.guest),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unrecognised tier granted access instead of failing closed"
    );

    // And it is refused as a non-member — 404, not 403 — so it cannot be probed for either.
    let (status, _) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{}/engine/v1/board", h.project),
        Some(&h.guest),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The creator is untouched: an unreadable row for one member must not break the project.
    let (status, body) = call(
        &h.app,
        "GET",
        &format!("/v1/projects/{}", h.project),
        Some(&h.creator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["tier"], "admin");
}

/// The tier is checked **before** the request body is parsed.
///
/// Found by probing a running server rather than by reading: a guest POSTing a malformed body to an
/// admin route got a 422 about their JSON instead of a 403 about their tier. Not a bypass — a valid
/// body always reached the tier check and was refused — but the wrong order twice over. It told a
/// caller about a route they may not use, and it deserialised attacker-controlled input before
/// deciding whether they were allowed to send any.
///
/// The fix is `AdminScope`, which moves the check into extraction, ahead of the body. This pins it:
/// the answer must be 403 whatever the body looks like, including no body at all.
#[tokio::test]
async fn the_tier_is_checked_before_the_body_is_parsed() {
    let h = harness().await;
    let bodies = [
        None,                              // no body at all
        Some(json!({})),                   // valid JSON, wrong shape
        Some(json!({"role": 7})),          // right key, wrong type
        Some(json!("not even an object")), // valid JSON, not an object
    ];

    for (label, body) in ["absent", "empty", "wrong type", "not an object"]
        .into_iter()
        .zip(bodies)
    {
        for (tier, token) in [("guest", &h.guest), ("prompter", &h.prompter)] {
            for uri in [
                format!("/v1/projects/{}/members", h.project),
                format!("/v1/projects/{}/invites", h.project),
                format!("/v1/projects/{}/board/apply", h.project),
            ] {
                let (status, _) = call(&h.app, "POST", &uri, Some(token), body.clone()).await;
                assert_eq!(
                    status,
                    StatusCode::FORBIDDEN,
                    "a {tier} sending a {label} body to {uri} learned about the body instead of \
                     being refused for their tier"
                );
            }
        }
    }

    // And an admin sending the same malformed bodies still gets a body error, not a tier error:
    // the check moved, it did not swallow the 400/422 that a real admin needs to see.
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/projects/{}/members", h.project),
        Some(&h.creator),
        Some(json!({})),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::FORBIDDEN,
        "an admin was refused by tier"
    );
    assert!(
        status.is_client_error(),
        "a malformed body should still be a client error for an admin, got {status}"
    );
}
