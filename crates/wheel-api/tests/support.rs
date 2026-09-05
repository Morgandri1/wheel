//! Test scaffolding: a throwaway RSA keypair, a JWKS server that counts fetches, and token minting
//! helpers that can produce *deliberately malformed* tokens.

use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub const KID: &str = "test-key-1";
pub const ISSUER: &str = "https://clerk.example.test";

fn b64u(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub struct TestKey {
    pub private_pem: String,
    pub public_der_b64: String,
    pub jwks: serde_json::Value,
}

/// A fixed 2048-bit test key, committed as a fixture.
///
/// Generating an RSA key at test time cost ~90 seconds, because the bignum arithmetic in the `rsa`
/// crate is unoptimised in a debug build. This is only ever a *test* key — it signs nothing real
/// and guards nothing — so baking it in trades zero security for a suite that runs in
/// milliseconds and stays cheap for QA to run on every change.
const TEST_PRIVATE_KEY_PEM: &str = include_str!("fixtures/test_rsa_key.pem");

pub fn make_key() -> TestKey {
    let private = RsaPrivateKey::from_pkcs1_pem(TEST_PRIVATE_KEY_PEM).expect("parse fixture key");
    let public = RsaPublicKey::from(&private);
    let private_pem = TEST_PRIVATE_KEY_PEM.to_string();

    let n = b64u(&public.n().to_bytes_be());
    let e = b64u(&public.e().to_bytes_be());

    let jwks = json!({
        "keys": [{
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "kid": KID,
            "n": n,
            "e": e,
        }]
    });

    TestKey {
        private_pem,
        // Used by the algorithm-confusion test as the attacker's guessed HMAC secret.
        public_der_b64: b64u(&public.n().to_bytes_be()),
        jwks,
    }
}

/// A JWKS endpoint that records how many times it was fetched, so we can assert the refresh
/// throttle actually throttles.
pub struct JwksServer {
    pub url: String,
    pub hits: Arc<AtomicUsize>,
    _handle: tokio::task::JoinHandle<()>,
}

pub async fn serve_jwks(body: serde_json::Value) -> JwksServer {
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let app = axum::Router::new().route(
        "/jwks",
        axum::routing::get(move || {
            let h = h.clone();
            let body = body.clone();
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                axum::Json(body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    JwksServer {
        url: format!("http://{addr}/jwks"),
        hits,
        _handle: handle,
    }
}

#[derive(serde::Serialize)]
pub struct Claims {
    pub sub: String,
    pub iss: String,
    pub exp: i64,
    pub nbf: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub azp: Option<String>,
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn claims(sub: &str) -> Claims {
    Claims {
        sub: sub.into(),
        iss: ISSUER.into(),
        exp: now() + 3600,
        nbf: now() - 60,
        azp: None,
    }
}

pub fn sign_rs256(key: &TestKey, kid: &str, c: &Claims) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.to_string());
    jsonwebtoken::encode(
        &header,
        c,
        &EncodingKey::from_rsa_pem(key.private_pem.as_bytes()).unwrap(),
    )
    .unwrap()
}

/// Sign with HS256 using an arbitrary secret, while still claiming a real `kid`.
/// This is the shape of the algorithm-confusion attack.
pub fn sign_hs256(kid: &str, secret: &[u8], c: &Claims) -> String {
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(kid.to_string());
    jsonwebtoken::encode(&header, c, &EncodingKey::from_secret(secret)).unwrap()
}

/// Hand-roll an `alg: none` token — no library will mint one for us.
pub fn forge_alg_none(c: &Claims) -> String {
    let header = b64u(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = b64u(serde_json::to_string(c).unwrap().as_bytes());
    format!("{header}.{payload}.")
}

/// Flip a byte in the payload while leaving the signature intact.
pub fn tamper(token: &str) -> String {
    let parts: Vec<&str> = token.split('.').collect();
    let mut payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .unwrap();
    let s = String::from_utf8(payload.clone()).unwrap();
    let swapped = s.replace("\"sub\":\"user_alice\"", "\"sub\":\"user_mallory\"");
    payload = swapped.into_bytes();
    format!("{}.{}.{}", parts[0], b64u(&payload), parts[2])
}
