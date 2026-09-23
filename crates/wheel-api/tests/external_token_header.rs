// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! `WHEEL_EXTERNAL_TOKEN_HEADER` means ONE door.
//!
//! A deployer behind an edge that signs its own assertion (Cloudflare Access's
//! `cf-access-jwt-assertion`) names that header, and from then on it is the *only* place a
//! credential is read. Falling back to `Authorization` or `x-auth-token` would let a caller choose
//! which of two doors to knock on, and a token an attacker can put in whichever header the
//! deployer did not think to watch is a token that skips every control on the watched one.
//!
//! Driven through the real router, because the property is about which request headers the
//! extractor consults — not observable from the verifier alone.

// Exercises the SQLite backend, so it exists only in a build that has one.
#![cfg(feature = "sqlite")]

mod support;

use axum::body::Body;
use axum::http::StatusCode;
use axum::Router;
use jsonwebtoken::Algorithm;
use serde_json::json;
use std::sync::Arc;
use support::*;
use tower::ServiceExt;
use wheel_api::config::{
    AuthMode, Config, Env, ExternalAuth, ExternalVerifier, Provision, SignupPolicy,
};
use wheel_api::crypto::Secret;
use wheel_api::db::Db;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

const DOOR: &str = "cf-access-jwt-assertion";

fn ext(url: &str) -> ExternalAuth {
    ExternalAuth {
        provider: "edge".into(),
        issuer: EXTERNAL_ISSUER.into(),
        audiences: vec![EXTERNAL_AUDIENCE.into()],
        sole_audience: false,
        allow_issuer_audience: false,
        subject_claim: "sub".into(),
        azp: vec![],
        max_ttl_secs: None,
        token_header: Some(DOOR.into()),
        provision: Provision::Auto,
        verifier: ExternalVerifier::Jwks {
            url: url.into(),
            algs: vec![Algorithm::RS256, Algorithm::EdDSA],
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

async fn app() -> (Router, String) {
    let key = make_key();
    let server = serve_jwks(external_jwks(&key)).await;
    let url = format!(
        "sqlite://{}",
        std::env::temp_dir()
            .join(format!("wheel-door-{}.db", uuid::Uuid::new_v4()))
            .display()
    );
    let db = Db::connect(&url).await.expect("connect and migrate");
    let external = ext(&server.url);
    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "https://clerk.test/jwks".into(),
            reqwest::Client::new(),
        ),
        external_jwks: Some(wheel_api::auth::jwks::JwksCache::new(
            server.url.clone(),
            reqwest::Client::new(),
        )),
        cfg: cfg(&url, external),
        db,
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(10_000),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(10_000, 10_000),
        engine_base_override: None,
        membership: wheel_api::membership::MembershipEvents::new(),
        bridges: wheel_api::http::bridges::BridgeCounter::new(),
    });
    let token = sign_rs256_value(
        &key,
        KID,
        &json!({
            "sub": "alice",
            "iss": EXTERNAL_ISSUER,
            "aud": EXTERNAL_AUDIENCE,
            "exp": now() + 300,
            "nbf": now() - 60,
            "iat": now(),
        }),
    );
    // Keep the JWKS server alive for the life of the test process.
    std::mem::forget(server);
    (wheel_api::build_router(state, &[]), token)
}

async fn status_with(app: &Router, headers: &[(&str, String)]) -> StatusCode {
    let mut req = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/projects");
    for (k, v) in headers {
        req = req.header(*k, v.as_str());
    }
    app.clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn the_named_header_is_the_only_door() {
    let (app, token) = app().await;

    // Positive control: the credential in the configured header is accepted. Without this the
    // refusals below would pass for any reason at all, including a broken fixture.
    assert_eq!(
        status_with(&app, &[(DOOR, token.clone())]).await,
        StatusCode::OK,
        "the configured header did not authenticate"
    );
    assert_eq!(
        status_with(&app, &[(DOOR, format!("Bearer {token}"))]).await,
        StatusCode::OK,
        "a Bearer-prefixed value in the configured header did not authenticate"
    );

    // The same, perfectly valid, credential anywhere else is not read at all.
    assert_eq!(
        status_with(&app, &[("authorization", format!("Bearer {token}"))]).await,
        StatusCode::UNAUTHORIZED,
        "Authorization was consulted although another header was configured"
    );
    assert_eq!(
        status_with(&app, &[("x-auth-token", token.clone())]).await,
        StatusCode::UNAUTHORIZED,
        "x-auth-token was consulted although another header was configured"
    );

    // And a bad value elsewhere does not spoil a good one in the configured header: the caller
    // does not get to choose a door, and the other doors are simply not consulted.
    assert_eq!(
        status_with(
            &app,
            &[
                (DOOR, token),
                ("authorization", "Bearer garbage".to_string())
            ]
        )
        .await,
        StatusCode::OK
    );
}
