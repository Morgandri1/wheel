// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! A runnable JWKS issuer, so `AUTH_MODE=jwks` and `AUTH_MODE=external` can be exercised without
//! an account anywhere.
//!
//! We have always tested `jwks` mode — `tests/support.rs` stands up an issuer in-process and five
//! suites run the real router against it. What nobody could do was point a *browser* at one. Web
//! cannot ship their side of the auth-mode work without running the mode, and this host has no
//! Clerk keys, so the mode they cannot run is the one whose code path decides who is logged in.
//!
//! It now serves two documents, because there are two modes and they are deliberately not the same
//! issuer:
//!
//!   * `/jwks` — RSA only, for `AUTH_MODE=jwks`, unchanged.
//!   * `/external/jwks` — RSA **and** Ed25519, for `AUTH_MODE=external`. Both, because the whole
//!     point of the external verifier is that the algorithm comes from the key rather than from the
//!     token, and a key set with only one algorithm in it cannot demonstrate that.
//!
//!     cargo run -p wheel-api --example stub-issuer          # port 9911
//!     PORT=9000 SUB=user_abc cargo run -p wheel-api --example stub-issuer
//!
//! `GET /token?sub=<id>&alg=<RS256|EdDSA>&aud=<audience>` mints more.
//!
//! IT VERIFIES ITSELF BEFORE IT SERVES. On startup it mints tokens and puts them through
//! `auth::claims::verify` and `auth::external::verify_token` — the real production verifiers, not
//! copies. If a JWKS shape, a claim set or a verifier ever drifts from what the API accepts, this
//! exits non-zero saying so, rather than handing anyone a token that fails somewhere less obvious.
//! That is also what keeps the *tests'* fixture and this runnable stub from diverging: both sign
//! with the same two committed keys.
//!
//! NEVER IN PRODUCTION. It is an example target, so it is not built into any shipped binary and is
//! not reachable from the library. It signs with keys committed to this repository in plain text:
//! anyone can mint any `sub`. `tests/support.rs` remains the authority on the fixture.

use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::json;
use std::sync::Arc;
use wheel_api::config::{AuthMode, Config, Env, ExternalAuth, ExternalVerifier, Provision, SignupPolicy};

/// Shared with `tests/support.rs` by including the same files, so a token this mints and a token
/// the suite mints are signed by one key.
const TEST_PRIVATE_KEY_PEM: &str = include_str!("../tests/fixtures/test_rsa_key.pem");
const TEST_ED25519_KEY_PEM: &str = include_str!("../tests/fixtures/test_ed25519_key.pem");

const KID: &str = "test-key-1";
const ED_KID: &str = "test-ed25519-1";
/// base64url of the raw 32-byte Ed25519 public key that belongs to the PEM above.
///
/// Hardcoded rather than derived because no dependency here parses an Ed25519 PEM — and it is safe
/// to hardcode precisely because `self_check` mints and verifies before serving: a value that did
/// not belong to the private key would fail the signature check and this would refuse to start.
const ED_PUBLIC_X: &str = "AmUKDFwwuIqAcFP-b5FYLoZLhrxLKWJTY4rBGsL4HJs";

const ISSUER: &str = "https://clerk.example.test";
/// A *different* issuer for the external plane, because `Config::cross_check` refuses to boot when
/// two verifiers are pinned to one issuer — two token populations that can stand in for each other.
const EXTERNAL_ISSUER: &str = "https://idp.example.test";
const EXTERNAL_AUDIENCE: &str = "wheel-stub";

fn b64u(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn rsa_jwk(public: &RsaPublicKey) -> serde_json::Value {
    json!({
        "kty": "RSA",
        "use": "sig",
        "alg": "RS256",
        "kid": KID,
        "n": b64u(&public.n().to_bytes_be()),
        "e": b64u(&public.e().to_bytes_be()),
    })
}

fn ed25519_jwk() -> serde_json::Value {
    json!({
        "kty": "OKP",
        "use": "sig",
        "alg": "EdDSA",
        "crv": "Ed25519",
        "kid": ED_KID,
        "x": ED_PUBLIC_X,
    })
}

fn mint(sub: &str, alg: Algorithm, aud: Option<&str>) -> String {
    let now = chrono::Utc::now().timestamp();
    let mut claims = json!({
        "sub": sub,
        "iss": if aud.is_some() { EXTERNAL_ISSUER } else { ISSUER },
        "exp": now + 60 * 60 * 12,
        "nbf": now - 60,
        "iat": now,
    });
    if let Some(aud) = aud {
        claims["aud"] = json!(aud);
    }

    let (kid, key) = match alg {
        Algorithm::EdDSA => (
            ED_KID,
            EncodingKey::from_ed_pem(TEST_ED25519_KEY_PEM.as_bytes())
                .expect("ed25519 fixture key parses"),
        ),
        _ => (
            KID,
            EncodingKey::from_rsa_pem(TEST_PRIVATE_KEY_PEM.as_bytes())
                .expect("rsa fixture key parses"),
        ),
    };
    let mut header = Header::new(alg);
    header.kid = Some(kid.to_string());
    jsonwebtoken::encode(&header, &claims, &key).expect("signing a token with our own key")
}

fn base_config(jwks_url: &str) -> Config {
    Config {
        env: Env::Dev,
        bind_addr: "127.0.0.1:0".into(),
        database_url: "sqlite://:memory:".into(),
        clerk_jwks_url: jwks_url.to_string(),
        clerk_issuer: ISSUER.into(),
        clerk_azp: vec![],
        dev_secret: None,
        auth_mode: AuthMode::Jwks,
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
        signup: SignupPolicy::Open,
        external: None,
        ws_max_bridges_per_project: 16,
        ws_max_lifetime_secs: 3600,
    }
}

fn external_config(url: &str) -> ExternalAuth {
    ExternalAuth {
        provider: "stub".into(),
        issuer: EXTERNAL_ISSUER.into(),
        audiences: vec![EXTERNAL_AUDIENCE.into()],
        sole_audience: false,
        subject_claim: "sub".into(),
        azp: vec![],
        max_ttl_secs: None,
        token_header: None,
        provision: Provision::Auto,
        verifier: ExternalVerifier::Jwks {
            url: url.to_string(),
            algs: vec![Algorithm::RS256, Algorithm::EdDSA],
        },
    }
}

/// Prove minted tokens pass the REAL verifiers before anyone depends on one.
///
/// Both planes, and both algorithms on the external one. The Ed25519 case is the one that matters
/// most: it is the whole reason the external verifier resolves an algorithm from the key set rather
/// than from the token header, and nothing else here would notice if that broke.
async fn self_check(jwks_url: &str, external_jwks_url: &str) -> Result<(), String> {
    let cfg = base_config(jwks_url);
    let cache = wheel_api::auth::jwks::JwksCache::new(jwks_url.to_string(), reqwest::Client::new());
    let token = mint("selfcheck", Algorithm::RS256, None);
    match wheel_api::auth::claims::verify(&token, &cfg, &cache).await {
        Ok(u) if u.user_id == "selfcheck" => {}
        Ok(u) => return Err(format!("verified as the wrong subject: {}", u.user_id)),
        Err(e) => return Err(format!("the API's own jwks verifier rejected our token: {e:?}")),
    }

    let ext = external_config(external_jwks_url);
    let ext_cache = wheel_api::auth::jwks::JwksCache::new(
        external_jwks_url.to_string(),
        reqwest::Client::new(),
    );
    for alg in [Algorithm::RS256, Algorithm::EdDSA] {
        let token = mint("selfcheck", alg, Some(EXTERNAL_AUDIENCE));
        match wheel_api::auth::external::verify_token(&token, &ext, &ext_cache).await {
            Ok(v) if v.subject == "selfcheck" => {}
            Ok(v) => return Err(format!("{alg:?}: verified as the wrong subject: {}", v.subject)),
            Err(e) => {
                return Err(format!(
                    "{alg:?}: the API's own external verifier rejected our token: {e:?}"
                ))
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9911);
    let sub = std::env::var("SUB").unwrap_or_else(|_| "user_stub".to_string());

    let private = RsaPrivateKey::from_pkcs1_pem(TEST_PRIVATE_KEY_PEM).expect("fixture key parses");
    let rsa = rsa_jwk(&RsaPublicKey::from(&private));
    let clerk_doc = Arc::new(json!({ "keys": [rsa.clone()] }));
    let external_doc = Arc::new(json!({ "keys": [rsa, ed25519_jwk()] }));

    let clerk = clerk_doc.clone();
    let ext = external_doc.clone();
    let app = axum::Router::new()
        .route(
            "/jwks",
            axum::routing::get(move || {
                let doc = clerk.clone();
                async move { axum::Json((*doc).clone()) }
            }),
        )
        .route(
            "/external/jwks",
            axum::routing::get(move || {
                let doc = ext.clone();
                async move { axum::Json((*doc).clone()) }
            }),
        )
        .route(
            "/token",
            axum::routing::get(
                |q: axum::extract::Query<std::collections::HashMap<String, String>>| async move {
                    let sub = q.get("sub").cloned().unwrap_or_else(|| "user_stub".into());
                    let alg = match q.get("alg").map(String::as_str) {
                        Some("EdDSA") | Some("eddsa") => Algorithm::EdDSA,
                        _ => Algorithm::RS256,
                    };
                    let aud = q.get("aud").cloned();
                    axum::Json(json!({
                        "sub": sub,
                        "token": mint(&sub, alg, aud.as_deref()),
                    }))
                },
            ),
        );

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap_or_else(|e| panic!("cannot bind 127.0.0.1:{port}: {e}"));
    let addr = listener.local_addr().expect("a bound address");
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let jwks_url = format!("http://{addr}/jwks");
    let external_jwks_url = format!("http://{addr}/external/jwks");
    if let Err(why) = self_check(&jwks_url, &external_jwks_url).await {
        eprintln!("stub-issuer refuses to serve: {why}");
        eprintln!("the fixture, a JWKS document or one of the API's verifiers have drifted apart.");
        std::process::exit(1);
    }

    println!("stub JWKS issuer — NOT FOR PRODUCTION, the signing keys are public in this repo");
    println!();
    println!("  AUTH_MODE=jwks");
    println!("  CLERK_JWKS_URL={jwks_url}");
    println!("  CLERK_ISSUER={ISSUER}");
    println!();
    println!("  x-auth-token: {}", mint(&sub, Algorithm::RS256, None));
    println!();
    println!("or the deployer-brings-their-own plane:");
    println!();
    println!("  AUTH_MODE=external");
    println!("  WHEEL_EXTERNAL_VERIFIER=jwks");
    println!("  WHEEL_EXTERNAL_JWKS_URL={external_jwks_url}");
    println!("  WHEEL_EXTERNAL_ISSUER={EXTERNAL_ISSUER}");
    println!("  WHEEL_EXTERNAL_AUDIENCE={EXTERNAL_AUDIENCE}");
    println!("  WHEEL_EXTERNAL_ALGS=RS256,EdDSA");
    println!("  WHEEL_EXTERNAL_PROVISION=auto");
    println!();
    println!(
        "  x-auth-token: {}",
        mint(&sub, Algorithm::EdDSA, Some(EXTERNAL_AUDIENCE))
    );
    println!();
    println!("  more tokens: curl 'http://{addr}/token?sub=<user id>&alg=EdDSA&aud={EXTERNAL_AUDIENCE}'");
    println!("  every one of them verified against the API's own verifiers before serving.");

    tokio::signal::ctrl_c().await.ok();
}
