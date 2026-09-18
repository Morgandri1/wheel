// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! A runnable JWKS issuer, so `AUTH_MODE=jwks` can be exercised without a Clerk account.
//!
//! We have always tested `jwks` mode — `tests/support.rs` stands up an issuer in-process and five
//! suites run the real router against it. What nobody could do was point a *browser* at one. Web
//! cannot ship their side of the auth-mode work without running the mode, and this host has no
//! Clerk keys, so the mode they cannot run is the one whose code path decides who is logged in.
//!
//! This serves the same fixture key over a real port.
//!
//!     cargo run -p wheel-api --example stub-issuer          # port 9911
//!     PORT=9000 SUB=user_abc cargo run -p wheel-api --example stub-issuer
//!
//! Then point the API at it:
//!
//!     AUTH_MODE=jwks CLERK_JWKS_URL=http://127.0.0.1:9911/jwks CLERK_ISSUER=https://clerk.example.test
//!
//! It prints a ready-to-use token. `GET /token?sub=<id>` mints more.
//!
//! IT VERIFIES ITSELF BEFORE IT SERVES. On startup it mints a token and puts it through
//! `wheel_api::auth::claims::verify` — the real production verifier, not a copy. If the JWKS shape
//! or the claims ever drift from what the API accepts, this exits non-zero saying so, rather than
//! handing Web a token that fails somewhere less obvious.
//!
//! NEVER IN PRODUCTION. It is an example target, so it is not built into any shipped binary and is
//! not reachable from the library. It signs with a key committed to this repository in plain text:
//! anyone can mint any `sub`. `tests/support.rs` remains the authority on the fixture.

use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::json;
use std::sync::Arc;

/// Shared with `tests/support.rs` by including the same file, so a token this mints and a token the
/// suite mints are signed by one key.
const TEST_PRIVATE_KEY_PEM: &str = include_str!("../tests/fixtures/test_rsa_key.pem");
const KID: &str = "test-key-1";
const ISSUER: &str = "https://clerk.example.test";

fn b64u(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn jwks_document(public: &RsaPublicKey) -> serde_json::Value {
    json!({
        "keys": [{
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "kid": KID,
            "n": b64u(&public.n().to_bytes_be()),
            "e": b64u(&public.e().to_bytes_be()),
        }]
    })
}

fn mint(sub: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    let claims = json!({
        "sub": sub,
        "iss": ISSUER,
        "exp": now + 60 * 60 * 12,
        "nbf": now - 60,
        "iat": now,
    });
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(KID.to_string());
    jsonwebtoken::encode(
        &header,
        &claims,
        &EncodingKey::from_rsa_pem(TEST_PRIVATE_KEY_PEM.as_bytes()).expect("fixture key parses"),
    )
    .expect("signing a token with our own key")
}

/// Prove a minted token passes the REAL verifier before anyone depends on one.
async fn self_check(jwks_url: &str) -> Result<(), String> {
    let cfg = wheel_api::config::Config {
        env: wheel_api::config::Env::Dev,
        bind_addr: "127.0.0.1:0".into(),
        database_url: "sqlite://:memory:".into(),
        clerk_jwks_url: jwks_url.to_string(),
        clerk_issuer: ISSUER.into(),
        clerk_azp: vec![],
        dev_secret: None,
        auth_mode: wheel_api::config::AuthMode::Jwks,
        session_secret: wheel_api::crypto::Secret::new(String::new()),
        master_key: [0u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: wheel_api::crypto::Secret::new("unused"),
        engine_port: 7000,
        public_base_url: "http://127.0.0.1".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 60,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
        signup: wheel_api::config::SignupPolicy::Open,
        ws_max_bridges_per_project: 16,
        ws_max_lifetime_secs: 3600,
    };
    let cache = wheel_api::auth::jwks::JwksCache::new(jwks_url.to_string(), reqwest::Client::new());
    let token = mint("selfcheck");
    match wheel_api::auth::claims::verify(&token, &cfg, &cache).await {
        Ok(u) if u.user_id == "selfcheck" => Ok(()),
        Ok(u) => Err(format!("verified as the wrong subject: {}", u.user_id)),
        Err(e) => Err(format!("the API's own verifier rejected our token: {e:?}")),
    }
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9911);
    let sub = std::env::var("SUB").unwrap_or_else(|_| "user_stub".to_string());

    let private = RsaPrivateKey::from_pkcs1_pem(TEST_PRIVATE_KEY_PEM).expect("fixture key parses");
    let jwks = Arc::new(jwks_document(&RsaPublicKey::from(&private)));

    let doc = jwks.clone();
    let app = axum::Router::new()
        .route(
            "/jwks",
            axum::routing::get(move || {
                let doc = doc.clone();
                async move { axum::Json((*doc).clone()) }
            }),
        )
        .route(
            "/token",
            axum::routing::get(
                |q: axum::extract::Query<std::collections::HashMap<String, String>>| async move {
                    let sub = q.get("sub").cloned().unwrap_or_else(|| "user_stub".into());
                    axum::Json(json!({ "sub": sub, "token": mint(&sub) }))
                },
            ),
        );

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap_or_else(|e| panic!("cannot bind 127.0.0.1:{port}: {e}"));
    let addr = listener.local_addr().expect("a bound address");
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let jwks_url = format!("http://{addr}/jwks");
    if let Err(why) = self_check(&jwks_url).await {
        eprintln!("stub-issuer refuses to serve: {why}");
        eprintln!("the fixture, the JWKS document or the API's verifier have drifted apart.");
        std::process::exit(1);
    }

    println!("stub JWKS issuer — NOT FOR PRODUCTION, the signing key is public in this repo");
    println!();
    println!("  AUTH_MODE=jwks");
    println!("  CLERK_JWKS_URL={jwks_url}");
    println!("  CLERK_ISSUER={ISSUER}");
    println!();
    println!("  x-auth-token: {}", mint(&sub));
    println!();
    println!("  more tokens: curl 'http://{addr}/token?sub=<user id>'");
    println!("  verified against the API's own verifier before serving.");

    tokio::signal::ctrl_c().await.ok();
}
