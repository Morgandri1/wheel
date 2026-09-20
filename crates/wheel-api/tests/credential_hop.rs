// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Under `proxy_header` auth, the proxy's subject and email headers are the credential — and a
//! credential must not cross the hop into an engine.
//!
//! `http::hop`'s unit test proves that `sanitize_for_upstream` drops the names it is *given*, and
//! `http::actor`'s proves that `credential_headers` derives the right names from configuration.
//! Neither proves the two are wired together at the call sites, and the mutation that removes the
//! argument from either call site survives both of them. This file is that missing assertion, taken
//! from the only place it is observable: what an engine actually receives.
//!
//! Both outbound paths, because they are two call sites:
//!
//!   * the **authenticated** engine proxy (`routes::proxy`), where relaying the header would let an
//!     agent read — and replay back at this API — the identity the edge asserted for its caller;
//!   * **public ingress** (`routes::ingress`), which authenticates nobody, so the header is not a
//!     credential *on that path* — but a deployment behind an authenticating proxy has one attached
//!     to every request the proxy forwards, ingress hits included.
//!
//! The mock engine echoes the headers it was handed, so the assertion is on what crossed rather
//! than on what we believe we sent.

// Exercises the SQLite backend, so it exists only in a build that has one.
#![cfg(feature = "sqlite")]

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::Router;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
use wheel_api::config::{
    AuthMode, Config, Env, ExternalAuth, ExternalVerifier, Provision, SignupPolicy,
};
use wheel_api::crypto::Secret;
use wheel_api::db::Db;
use wheel_api::http::client_ip::TrustedPeer;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

const SUBJECT_HEADER: &str = "x-forwarded-user";
const EMAIL_HEADER: &str = "x-forwarded-email";

/// An engine that answers every request with the headers it was given. What crossed the hop is
/// then a fact in the response body rather than an inference from the API's own account of it.
async fn echoing_engine() -> String {
    let app = Router::new().fallback(|headers: HeaderMap, _req: Request<Body>| async move {
        let seen: Vec<String> = headers
            .keys()
            .map(|k| k.as_str().to_ascii_lowercase())
            .collect();
        axum::Json(json!({ "seen": seen }))
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn proxy_external() -> ExternalAuth {
    ExternalAuth {
        provider: "oauth2-proxy".into(),
        issuer: "proxy:oauth2-proxy".into(),
        audiences: vec!["wheel-test".into()],
        sole_audience: false,
        allow_issuer_audience: false,
        subject_claim: "sub".into(),
        azp: vec![],
        max_ttl_secs: None,
        token_header: None,
        provision: Provision::Auto,
        verifier: ExternalVerifier::ProxyHeader {
            subject_header: SUBJECT_HEADER.into(),
            email_header: Some(EMAIL_HEADER.into()),
        },
    }
}

fn cfg(db_url: &str, external: ExternalAuth) -> Config {
    Config {
        env: Env::Prod,
        bind_addr: "127.0.0.1:0".into(),
        database_url: db_url.into(),
        jwks_url: "https://clerk.test/jwks".into(),
        jwks_issuer: "https://clerk.test".into(),
        jwks_azp: vec![],
        dev_secret: None,
        auth_mode: AuthMode::External,
        session_secret: Secret::new("session-secret-that-is-at-least-32-chars"),
        signup: SignupPolicy::Closed,
        master_key: [7u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "https://api.wheel.test".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 10_000,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
        external: Some(external),
        ws_max_bridges_per_project: 16,
        ws_max_lifetime_secs: 3600,
    }
}

async fn app() -> Router {
    app_with(proxy_external()).await
}

async fn app_with(external: ExternalAuth) -> Router {
    let path = std::env::temp_dir().join(format!("wheel-hop-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let db = Db::connect(&url).await.expect("connect and migrate");
    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "https://clerk.test/jwks".into(),
            reqwest::Client::new(),
        ),
        cfg: cfg(&url, external),
        db,
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(10_000),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(10_000, 10_000),
        engine_base_override: Some(echoing_engine().await),
        external_jwks: None,
        membership: wheel_api::membership::MembershipEvents::new(),
        bridges: wheel_api::http::bridges::BridgeCounter::new(),
    });
    wheel_api::build_router(state, &[])
}

/// Every request here is made the way the proxy makes it: the identity headers attached, and the
/// server-side `TrustedPeer` marker set, which a client can never forge.
async fn as_the_proxy(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    as_the_proxy_with(app, method, uri, body, &[]).await
}

/// As above, plus extra request headers — the deployer's own credential header among them.
async fn as_the_proxy_with(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    extra: &[(&str, &str)],
) -> (StatusCode, serde_json::Value) {
    let mut req = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header(SUBJECT_HEADER, "alice")
        .header(EMAIL_HEADER, "alice@corp.test");
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    let req = req;
    let mut req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    req.extensions_mut().insert(TrustedPeer);
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// The engine's report of what it received.
fn seen(body: &serde_json::Value) -> Vec<String> {
    body["seen"]
        .as_array()
        .unwrap_or_else(|| panic!("the engine did not answer with its headers: {body}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// A project owned by the proxy-asserted caller, with ingress opened.
async fn project(app: &Router) -> String {
    let (status, body) =
        as_the_proxy(app, "POST", "/v1/projects", Some(json!({"name": "hop"}))).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "could not create a project: {body}"
    );
    let id = body["id"].as_str().unwrap().to_string();

    let (status, body) = as_the_proxy(
        app,
        "PATCH",
        &format!("/v1/projects/{id}"),
        Some(json!({"capabilities": {"http": true}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "could not open ingress: {body}");
    id
}

#[tokio::test]
async fn the_proxy_assertion_never_reaches_an_engine_through_the_authenticated_proxy() {
    let app = app().await;
    let id = project(&app).await;

    let (status, body) = as_the_proxy(
        &app,
        "GET",
        &format!("/v1/projects/{id}/engine/v1/board"),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the engine proxy did not answer: {body}"
    );

    let seen = seen(&body);
    assert!(
        !seen.iter().any(|h| h == SUBJECT_HEADER),
        "the proxy-asserted subject reached the engine: {seen:?}"
    );
    assert!(
        !seen.iter().any(|h| h == EMAIL_HEADER),
        "the proxy-asserted email reached the engine: {seen:?}"
    );

    // Positive control. If nothing at all crossed, the two assertions above are vacuous — a mock
    // that never ran, or a request that never reached it, would pass them both.
    assert!(
        seen.iter().any(|h| h == "x-wheel-actor-id"),
        "the API's own actor headers did not cross either, so nothing here is evidence: {seen:?}"
    );
}

#[tokio::test]
async fn the_proxy_assertion_never_reaches_an_engine_through_public_ingress() {
    let app = app().await;
    let id = project(&app).await;

    // Ingress authenticates nobody, so this is the unauthenticated shape — but it still carries the
    // headers, because the deployment's proxy attaches them to everything it forwards.
    let req = axum::http::Request::builder()
        .method("GET")
        .uri(format!("/p/{id}/hook"))
        .header(SUBJECT_HEADER, "alice")
        .header(EMAIL_HEADER, "alice@corp.test")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "ingress did not reach the engine"
    );
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let seen = seen(&body);
    assert!(
        !seen.iter().any(|h| h == SUBJECT_HEADER),
        "the proxy-asserted subject reached the engine through ingress: {seen:?}"
    );
    assert!(
        !seen.iter().any(|h| h == EMAIL_HEADER),
        "the proxy-asserted email reached the engine through ingress: {seen:?}"
    );
    assert!(
        seen.iter().any(|h| h == "x-wheel-ingress"),
        "ingress's own marker did not cross, so nothing here is evidence: {seen:?}"
    );
}

/// The deployer's own credential header (`WHEEL_EXTERNAL_TOKEN_HEADER`, e.g. Cloudflare Access's
/// `cf-access-jwt-assertion`) carries a signed identity token. It was on no never-relay list, so it
/// reached the engine on both outbound paths — and through public ingress into a stored,
/// guest-readable message body. Required change 4 on #136.
const TOKEN_HEADER: &str = "cf-access-jwt-assertion";

fn with_token_header() -> ExternalAuth {
    ExternalAuth {
        token_header: Some(TOKEN_HEADER.into()),
        ..proxy_external()
    }
}

#[tokio::test]
async fn the_deployers_token_header_never_reaches_an_engine_through_the_authenticated_proxy() {
    let app = app_with(with_token_header()).await;
    let id = project(&app).await;

    let (status, body) = as_the_proxy_with(
        &app,
        "GET",
        &format!("/v1/projects/{id}/engine/v1/board"),
        None,
        &[(TOKEN_HEADER, "eyJ.signed.identity-token")],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the engine proxy did not answer: {body}"
    );

    let seen = seen(&body);
    assert!(
        !seen.iter().any(|h| h == TOKEN_HEADER),
        "the deployer's credential header reached the engine: {seen:?}"
    );
    assert!(
        seen.iter().any(|h| h == "x-wheel-actor-id"),
        "nothing crossed, so the assertion above is vacuous: {seen:?}"
    );
}

#[tokio::test]
async fn the_deployers_token_header_never_reaches_an_engine_through_public_ingress() {
    let app = app_with(with_token_header()).await;
    let id = project(&app).await;

    let req = axum::http::Request::builder()
        .method("GET")
        .uri(format!("/p/{id}/hook"))
        .header(TOKEN_HEADER, "eyJ.signed.identity-token")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "ingress did not reach the engine"
    );
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let seen = seen(&body);
    assert!(
        !seen.iter().any(|h| h == TOKEN_HEADER),
        "the deployer's credential header reached the engine through ingress: {seen:?}"
    );
    assert!(
        seen.iter().any(|h| h == "x-wheel-ingress"),
        "nothing crossed, so the assertion above is vacuous: {seen:?}"
    );
}
