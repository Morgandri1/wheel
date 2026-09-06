//! The auth negative suite.
//!
//! These tests target `verify()` directly rather than going through the router, so they need no
//! database and run in milliseconds. Ownership/404 behaviour is covered separately in the
//! database-backed integration tests.
//!
//! Four of these are here because ADVERSARY said it would try exactly them:
//! `alg: none`, HS256-with-the-RSA-public-key confusion, unknown-`kid` refresh flooding, and
//! `Authorization` vs `x-auth-token` precedence.

mod support;

use axum::http::HeaderMap;
use std::sync::OnceLock;
use support::*;
use wheel_api::auth::claims::{token_from_headers, verify};
use wheel_api::auth::jwks::JwksCache;
use wheel_api::config::{Config, Env};

fn key() -> &'static TestKey {
    // RSA keygen is the expensive part; generate one key for the whole suite.
    static K: OnceLock<TestKey> = OnceLock::new();
    K.get_or_init(make_key)
}

fn config(env: Env, jwks_url: &str, dev_secret: Option<&str>) -> Config {
    Config {
        env,
        bind_addr: "127.0.0.1:0".into(),
        database_url: "postgres://unused".into(),
        clerk_jwks_url: jwks_url.into(),
        clerk_issuer: ISSUER.into(),
        clerk_azp: vec![],
        dev_secret: dev_secret.map(str::to_string),
        auth_mode: wheel_api::config::AuthMode::Local,
        session_secret: wheel_api::crypto::Secret::new("test-session-secret-at-least-32-chars"),
        master_key: [0u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: wheel_api::crypto::Secret::new("unused"),
        engine_port: 7000,
        public_base_url: "http://localhost".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 60,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
    }
}

async fn fixture(env: Env, dev_secret: Option<&str>) -> (Config, JwksCache, JwksServer) {
    let server = serve_jwks(key().jwks.clone()).await;
    let cfg = config(env, &server.url, dev_secret);
    let cache = JwksCache::new(cfg.clerk_jwks_url.clone(), reqwest::Client::new());
    (cfg, cache, server)
}

// ---------------------------------------------------------------- the happy path

#[tokio::test]
async fn valid_token_is_accepted() {
    let (cfg, jwks, _s) = fixture(Env::Prod, None).await;
    let t = sign_rs256(key(), KID, &claims("user_alice"));
    let u = verify(&t, &cfg, &jwks).await.expect("valid token rejected");
    assert_eq!(u.user_id, "user_alice");
}

// ---------------------------------------------------------------- the four ADVERSARY named

#[tokio::test]
async fn alg_none_is_rejected() {
    let (cfg, jwks, _s) = fixture(Env::Prod, None).await;
    let t = forge_alg_none(&claims("user_alice"));
    assert!(
        verify(&t, &cfg, &jwks).await.is_err(),
        "an unsigned alg:none token was accepted"
    );
}

#[tokio::test]
async fn hs256_signed_with_rsa_public_key_is_rejected() {
    // The classic algorithm-confusion attack: the RSA public key is public, so if a verifier can be
    // talked into treating the token as HMAC, the attacker can sign tokens with a key they already
    // have. We must never reach an HMAC verifier for an RS256-issued tenant.
    let (cfg, jwks, _s) = fixture(Env::Prod, None).await;
    let t = sign_hs256(
        KID,
        key().public_der_b64.as_bytes(),
        &claims("user_mallory"),
    );
    assert!(
        verify(&t, &cfg, &jwks).await.is_err(),
        "ALGORITHM CONFUSION: token signed with the public key as an HMAC secret was accepted"
    );
}

#[tokio::test]
async fn hs256_confusion_still_rejected_in_dev_mode() {
    // Even in dev, where an HS256 path exists, it verifies against the *dev secret* — not against
    // anything derived from the RSA key.
    let (cfg, jwks, _s) = fixture(Env::Dev, Some("the-real-dev-secret")).await;
    let t = sign_hs256(
        KID,
        key().public_der_b64.as_bytes(),
        &claims("user_mallory"),
    );
    assert!(verify(&t, &cfg, &jwks).await.is_err());
}

#[tokio::test]
async fn unknown_kid_flood_does_not_flood_the_jwks_endpoint() {
    // An attacker can mint tokens bearing arbitrary `kid`s for free. If each one triggered a
    // refetch, our auth path would become a traffic pump aimed at Clerk and a way to stall our own
    // handlers. The throttle must hold the fetch count down regardless of request volume.
    let (cfg, jwks, server) = fixture(Env::Prod, None).await;

    for i in 0..50 {
        let t = sign_rs256(key(), &format!("attacker-kid-{i}"), &claims("user_mallory"));
        assert!(
            verify(&t, &cfg, &jwks).await.is_err(),
            "unknown kid was accepted"
        );
    }

    let hits = server.hits.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        hits <= 1,
        "JWKS was fetched {hits} times for 50 unknown kids; the once-per-minute throttle is not holding"
    );
}

#[tokio::test]
async fn x_auth_token_takes_precedence_over_authorization() {
    // Two credential sources must resolve deterministically. If they disagreed silently, an
    // attacker could pair a victim's header with their own and hope the wrong one is authorised.
    let mut h = HeaderMap::new();
    h.insert("x-auth-token", "primary-token".parse().unwrap());
    h.insert("authorization", "Bearer secondary-token".parse().unwrap());
    assert_eq!(token_from_headers(&h), Some("primary-token"));
}

// ---------------------------------------------------------------- claim validation

#[tokio::test]
async fn expired_token_is_rejected() {
    let (cfg, jwks, _s) = fixture(Env::Prod, None).await;
    let mut c = claims("user_alice");
    c.exp = now() - 10;
    assert!(verify(&sign_rs256(key(), KID, &c), &cfg, &jwks)
        .await
        .is_err());
}

#[tokio::test]
async fn not_yet_valid_token_is_rejected() {
    let (cfg, jwks, _s) = fixture(Env::Prod, None).await;
    let mut c = claims("user_alice");
    c.nbf = now() + 600;
    assert!(verify(&sign_rs256(key(), KID, &c), &cfg, &jwks)
        .await
        .is_err());
}

#[tokio::test]
async fn wrong_issuer_is_rejected() {
    let (cfg, jwks, _s) = fixture(Env::Prod, None).await;
    let mut c = claims("user_alice");
    c.iss = "https://evil.example".into();
    assert!(verify(&sign_rs256(key(), KID, &c), &cfg, &jwks)
        .await
        .is_err());
}

#[tokio::test]
async fn tampered_payload_is_rejected() {
    let (cfg, jwks, _s) = fixture(Env::Prod, None).await;
    let good = sign_rs256(key(), KID, &claims("user_alice"));
    let bad = tamper(&good);
    assert_ne!(good, bad, "tamper helper did not change the token");
    assert!(
        verify(&bad, &cfg, &jwks).await.is_err(),
        "payload was swapped to another user and still verified"
    );
}

#[tokio::test]
async fn empty_sub_is_rejected() {
    let (cfg, jwks, _s) = fixture(Env::Prod, None).await;
    let c = claims("");
    assert!(verify(&sign_rs256(key(), KID, &c), &cfg, &jwks)
        .await
        .is_err());
}

#[tokio::test]
async fn azp_outside_the_allowlist_is_rejected() {
    let (mut cfg, jwks, _s) = fixture(Env::Prod, None).await;
    cfg.clerk_azp = vec!["https://wheel.dev".into()];
    let mut c = claims("user_alice");
    c.azp = Some("https://evil.example".into());
    assert!(verify(&sign_rs256(key(), KID, &c), &cfg, &jwks)
        .await
        .is_err());

    c.azp = Some("https://wheel.dev".into());
    assert!(verify(&sign_rs256(key(), KID, &c), &cfg, &jwks)
        .await
        .is_ok());
}

#[tokio::test]
async fn garbage_tokens_are_rejected_without_panicking() {
    let (cfg, jwks, _s) = fixture(Env::Prod, None).await;
    for t in [
        "",
        ".",
        "..",
        "a.b.c",
        "not-a-jwt",
        "Bearer x",
        &"A".repeat(10_000),
    ] {
        assert!(
            verify(t, &cfg, &jwks).await.is_err(),
            "accepted garbage: {t:?}"
        );
    }
}

// ---------------------------------------------------------------- header extraction

#[test]
fn token_extraction_shapes() {
    let mk = |k: &str, v: &str| {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            v.parse().unwrap(),
        );
        h
    };

    assert_eq!(token_from_headers(&mk("x-auth-token", "t")), Some("t"));
    assert_eq!(
        token_from_headers(&mk("x-auth-token", "Bearer t")),
        Some("t")
    );
    assert_eq!(
        token_from_headers(&mk("authorization", "Bearer t")),
        Some("t")
    );
    // Scheme match is case-insensitive per RFC 7235.
    assert_eq!(
        token_from_headers(&mk("authorization", "bearer t")),
        Some("t")
    );
    assert_eq!(
        token_from_headers(&mk("authorization", "BEARER t")),
        Some("t")
    );
    // Non-bearer schemes must not be mistaken for one.
    assert_eq!(
        token_from_headers(&mk("authorization", "Basic dXNlcjpwdw==")),
        None
    );
    assert_eq!(token_from_headers(&mk("x-auth-token", "")), None);
    assert_eq!(token_from_headers(&mk("x-auth-token", "   ")), None);
    assert_eq!(token_from_headers(&HeaderMap::new()), None);
}
