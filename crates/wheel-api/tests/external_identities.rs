// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The operator's levers over external auth, driven through the real router.
//!
//! Two things are being proven, and the second is the one that would otherwise be discovered in
//! production:
//!
//!   1. Only the operator account may see or change who is who, and the routes do not exist at all
//!      unless `AUTH_MODE=external`.
//!   2. **A `wht_` API token still authenticates under `proxy_header` mode.** That mode has no
//!      bearer token of its own, so it is exactly the configuration in which an "authenticate by
//!      header" branch can swallow every other credential — and a `wht_` token is the only one an
//!      operator can use from a script. The operator's requirement is that `local` and `wht_` keep
//!      working; this is where that is checked for the awkward mode rather than the easy one.

// Exercises the SQLite backend, so it exists only in a build that has one.
#![cfg(feature = "sqlite")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
use wheel_api::config::{AuthMode, Config, Env, ExternalAuth, ExternalVerifier, Provision, SignupPolicy};
use wheel_api::crypto::Secret;
use wheel_api::db::Db;
use wheel_api::http::client_ip::TrustedPeer;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

const ISSUER: &str = "proxy:oauth2-proxy";

fn cfg(db_url: &str, external: Option<ExternalAuth>) -> Config {
    Config {
        env: Env::Prod,
        bind_addr: "127.0.0.1:0".into(),
        database_url: db_url.into(),
        clerk_jwks_url: "https://clerk.test/jwks".into(),
        clerk_issuer: "https://clerk.test".into(),
        clerk_azp: vec![],
        dev_secret: None,
        auth_mode: if external.is_some() {
            AuthMode::External
        } else {
            AuthMode::Local
        },
        session_secret: Secret::new("session-secret-that-is-at-least-32-chars"),
        signup: SignupPolicy::Open,
        master_key: [5u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "https://api.wheel.test".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 10_000,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
        external,
        ws_max_bridges_per_project: 16,
        ws_max_lifetime_secs: 3600,
    }
}

fn proxy_external() -> ExternalAuth {
    ExternalAuth {
        provider: "oauth2-proxy".into(),
        issuer: ISSUER.into(),
        audiences: vec!["wheel-test".into()],
        sole_audience: false,
        subject_claim: "sub".into(),
        azp: vec![],
        max_ttl_secs: None,
        token_header: None,
        provision: Provision::Auto,
        verifier: ExternalVerifier::ProxyHeader {
            subject_header: "x-forwarded-user".into(),
            email_header: Some("x-forwarded-email".into()),
        },
    }
}

async fn app(external: Option<ExternalAuth>) -> (Router, Db) {
    let path = std::env::temp_dir().join(format!("wheel-extid-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let db = Db::connect(&url).await.expect("connect and migrate");
    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "https://clerk.test/jwks".into(),
            reqwest::Client::new(),
        ),
        cfg: cfg(&url, external),
        db: db.clone(),
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(10_000),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(10_000, 10_000),
        engine_base_override: None,
        external_jwks: None,
        membership: wheel_api::membership::MembershipEvents::new(),
        bridges: wheel_api::http::bridges::BridgeCounter::new(),
    });
    (wheel_api::build_router(state, &[]), db)
}

/// A request, optionally carrying the server-side marker that says the TCP peer is a trusted proxy.
/// A client can never set that — it is a request extension — which is the whole point.
async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
    headers: &[(&str, &str)],
    trusted_peer: bool,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("x-auth-token", t);
    }
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    if trusted_peer {
        req.extensions_mut().insert(TrustedPeer);
    }
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

/// The owner account plus a `wht_` token for it — the credential `wheeld` hands an operator on first
/// boot, and the only one usable from a script.
async fn operator(db: &Db) -> String {
    wheel_api::auth::api_token::bootstrap_owner(db, "owner@example.com", "operator")
        .await
        .expect("bootstrap")
        .expect("a first-boot owner")
        .token
}

// --------------------------------------------------------------------------- wht_ under proxy mode

/// The regression this file exists for: a header-authenticating mode must not swallow the API-token
/// path. Without the ordering fix in `extractor.rs`, this request 401s and `wheeld token` is dead
/// on every proxy-authenticated deployment.
#[tokio::test]
async fn an_api_token_still_authenticates_under_proxy_header_mode() {
    let (app, db) = app(Some(proxy_external())).await;
    let token = operator(&db).await;

    // No proxy headers, no trusted peer — just the token, as a script would send it.
    let (status, body) = call(&app, "GET", "/v1/auth/me", Some(&token), None, &[], false).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a wht_ token was refused under proxy_header mode: {body}"
    );
    assert_eq!(body["email"], "owner@example.com");
    assert_eq!(body["owner"], true);
}

/// And the proxy path still works beside it, for a caller who has no token at all.
#[tokio::test]
async fn a_trusted_proxy_assertion_authenticates_and_provisions() {
    let (app, _db) = app(Some(proxy_external())).await;
    let (status, body) = call(
        &app,
        "GET",
        "/v1/projects",
        None,
        None,
        &[("x-forwarded-user", "alice"), ("x-forwarded-email", "alice@corp.test")],
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.as_array().unwrap().is_empty());
}

/// The control, and the reason the mode is safe to ship at all: the identical request without the
/// trusted-peer marker is refused. A client cannot manufacture the marker.
#[tokio::test]
async fn the_same_assertion_from_an_untrusted_peer_is_refused() {
    let (app, _db) = app(Some(proxy_external())).await;
    let (status, _) = call(
        &app,
        "GET",
        "/v1/projects",
        None,
        None,
        &[("x-forwarded-user", "alice")],
        false,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a header from an untrusted peer was believed"
    );
}

/// An external session may not launder its lifetime into a long-lived credential.
#[tokio::test]
async fn an_external_session_may_not_mint_an_api_token() {
    let (app, _db) = app(Some(proxy_external())).await;
    let (status, _) = call(
        &app,
        "POST",
        "/v1/auth/tokens",
        None,
        Some(json!({"name": "laundered"})),
        &[("x-forwarded-user", "alice")],
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// --------------------------------------------------------------------------- the admin routes

#[tokio::test]
async fn only_the_operator_account_may_manage_identities() {
    let (app, db) = app(Some(proxy_external())).await;
    let token = operator(&db).await;

    // The operator can list.
    let (status, body) = call(&app, "GET", "/v1/auth/external-identities", Some(&token), None, &[], false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.as_array().unwrap().is_empty());

    // An ordinary external principal cannot, even though it authenticates fine.
    let (status, _) = call(
        &app,
        "GET",
        "/v1/auth/external-identities",
        None,
        None,
        &[("x-forwarded-user", "alice")],
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Linking, and what it is for: `Provision::Linked` refuses an unknown subject until an operator
/// says which account it belongs to.
#[tokio::test]
async fn an_operator_can_link_and_then_disable_an_identity() {
    let mut ext = proxy_external();
    ext.provision = Provision::Linked;
    let (app, db) = app(Some(ext)).await;
    let token = operator(&db).await;

    // Before linking, the assertion is refused however trusted the peer is.
    let (status, _) = call(
        &app,
        "GET",
        "/v1/projects",
        None,
        None,
        &[("x-forwarded-user", "alice")],
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "linked mode provisioned a stranger");

    // The operator links the subject to an account that exists.
    let account = wheel_api::auth::local::create_user(&db, "alice@corp.test", "Correct-Horse-9!")
        .await
        .unwrap();
    let (status, linked) = call(
        &app,
        "POST",
        "/v1/auth/external-identities",
        Some(&token),
        Some(json!({"subject": "alice", "user_id": account.id, "email": "alice@corp.test"})),
        &[],
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{linked}");
    assert_eq!(linked["issuer"], ISSUER, "the issuer comes from configuration, not the request");
    assert_eq!(linked["user_id"], json!(account.id));

    // Now the same assertion authenticates, as that account.
    let (status, body) = call(
        &app,
        "GET",
        "/v1/auth/me",
        None,
        None,
        &[("x-forwarded-user", "alice")],
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], json!(account.id));

    // Disabling it is the operator's only revocation lever over a provider with no back-channel.
    let id = linked["id"].as_str().unwrap().to_string();
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/v1/auth/external-identities/{id}"),
        Some(&token),
        None,
        &[],
        false,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = call(
        &app,
        "GET",
        "/v1/auth/me",
        None,
        None,
        &[("x-forwarded-user", "alice")],
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a disabled identity still authenticated");

    // The row survives, so an operator can still see what they disabled.
    let (_, listed) = call(&app, "GET", "/v1/auth/external-identities", Some(&token), None, &[], false).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert!(listed[0]["disabled_at"].is_string());
}

/// An externally-provisioned account is **not** the owner account.
///
/// Both carry a sentinel in `password_hash` rather than a real argon2 hash, and if they carried the
/// *same* one then `is_token_only` — which gates account creation and the identity admin routes —
/// would be true for every user the deployer's IdP vouches for. That is a privilege escalation
/// handed out at login, so it gets its own test rather than being implied by the one above.
#[tokio::test]
async fn an_externally_provisioned_account_is_not_the_owner_account() {
    let (app, db) = app(Some(proxy_external())).await;
    // The owner exists, so "nobody is the owner" is not the reason this passes.
    let _ = operator(&db).await;

    let (status, me) = call(
        &app,
        "GET",
        "/v1/auth/me",
        None,
        None,
        &[("x-forwarded-user", "alice")],
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["owner"], false, "an external account was reported as the owner");

    // And it cannot use the owner-only routes, which is what `owner` actually controls.
    for (method, uri, body) in [
        ("GET", "/v1/auth/external-identities", None),
        (
            "POST",
            "/v1/auth/users",
            Some(json!({"email": "new@corp.test", "password": "Correct-Horse-9!"})),
        ),
    ] {
        let (status, _) = call(
            &app,
            method,
            uri,
            None,
            body,
            &[("x-forwarded-user", "alice")],
            true,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "an external account reached {uri}");
    }
}

/// Linking to an account that does not exist would create a row authenticating nobody, silently.
#[tokio::test]
async fn linking_to_an_absent_account_is_refused() {
    let (app, db) = app(Some(proxy_external())).await;
    let token = operator(&db).await;
    let (status, _) = call(
        &app,
        "POST",
        "/v1/auth/external-identities",
        Some(&token),
        Some(json!({"subject": "alice", "user_id": uuid::Uuid::new_v4()})),
        &[],
        false,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A subject that is not a principal must not reach the database, where it would later become an
/// actor header and an envelope attribute.
#[tokio::test]
async fn linking_a_hostile_subject_is_refused() {
    let (app, db) = app(Some(proxy_external())).await;
    let token = operator(&db).await;
    let account = wheel_api::auth::local::create_user(&db, "a@corp.test", "Correct-Horse-9!")
        .await
        .unwrap();
    for hostile in ["alice\" type=\"user", "alice bob", "alice\nx: y", ""] {
        let (status, _) = call(
            &app,
            "POST",
            "/v1/auth/external-identities",
            Some(&token),
            Some(json!({"subject": hostile, "user_id": account.id})),
            &[],
            false,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{hostile:?} was accepted as a subject"
        );
    }
}

/// The routes do not exist at all unless the deployment uses external auth. A surface that is
/// merely "empty" under local auth is still a surface.
#[tokio::test]
async fn the_identity_routes_are_absent_without_external_auth() {
    let (app, db) = app(None).await;
    let token = operator(&db).await;
    let (status, _) = call(&app, "GET", "/v1/auth/external-identities", Some(&token), None, &[], false).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = call(
        &app,
        "POST",
        "/v1/auth/external-identities",
        Some(&token),
        Some(json!({"subject": "alice", "user_id": uuid::Uuid::new_v4()})),
        &[],
        false,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
