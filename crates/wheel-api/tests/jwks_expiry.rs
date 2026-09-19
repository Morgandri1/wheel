// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! A signing key the issuer has REMOVED must stop verifying.
//!
//! The cache used to hold a key for the life of the process: it refetched only on an unknown `kid`,
//! so a token signed by a key the issuer had since withdrawn — because it leaked, say — verified
//! until Wheel restarted. This drives a JWKS server whose key set changes and breaks, with the time
//! bounds shrunk so the test does not wait minutes.

mod support;

use axum::http::{header, StatusCode};
use jsonwebtoken::Algorithm;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::*;
use wheel_api::auth::external;
use wheel_api::auth::jwks::{JwksCache, Timing};
use wheel_api::config::{ExternalAuth, ExternalVerifier, Provision};

struct Issuer {
    url: String,
    body: Arc<Mutex<serde_json::Value>>,
    failing: Arc<Mutex<bool>>,
    cache_control: Arc<Mutex<Option<String>>>,
    hits: Arc<std::sync::atomic::AtomicUsize>,
}

async fn issuer(initial: serde_json::Value) -> Issuer {
    let body = Arc::new(Mutex::new(initial));
    let failing = Arc::new(Mutex::new(false));
    let cache_control = Arc::new(Mutex::new(None::<String>));
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (b, f, c, h) = (
        body.clone(),
        failing.clone(),
        cache_control.clone(),
        hits.clone(),
    );
    let app = axum::Router::new().route(
        "/jwks",
        axum::routing::get(move || {
            let (b, f, c, h) = (b.clone(), f.clone(), c.clone(), h.clone());
            async move {
                h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if *f.lock().unwrap() {
                    return (StatusCode::INTERNAL_SERVER_ERROR, String::new())
                        .into_response_parts();
                }
                let mut headers = vec![];
                if let Some(v) = c.lock().unwrap().clone() {
                    headers.push((header::CACHE_CONTROL, v));
                }
                (StatusCode::OK, b.lock().unwrap().to_string(), headers).into_response_parts()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Issuer {
        url: format!("http://{addr}/jwks"),
        body,
        failing,
        cache_control,
        hits,
    }
}

trait Parts {
    fn into_response_parts(self) -> axum::response::Response;
}
impl Parts for (StatusCode, String) {
    fn into_response_parts(self) -> axum::response::Response {
        axum::response::IntoResponse::into_response(self)
    }
}
impl Parts for (StatusCode, String, Vec<(header::HeaderName, String)>) {
    fn into_response_parts(self) -> axum::response::Response {
        let mut res = axum::response::IntoResponse::into_response((
            self.0,
            [(header::CONTENT_TYPE, "application/json".to_string())],
            self.1,
        ));
        for (k, v) in self.2 {
            res.headers_mut().insert(k, v.parse().unwrap());
        }
        res
    }
}

const MAX_AGE: Duration = Duration::from_millis(300);
const GRACE: Duration = Duration::from_millis(300);

fn quick() -> Timing {
    Timing {
        min_refresh: Duration::ZERO,
        default_max_age: MAX_AGE,
        stale_grace: GRACE,
    }
}

fn ext(url: &str) -> ExternalAuth {
    ExternalAuth {
        provider: "test".into(),
        issuer: EXTERNAL_ISSUER.into(),
        audiences: vec![EXTERNAL_AUDIENCE.into()],
        sole_audience: false,
        allow_issuer_audience: false,
        subject_claim: "sub".into(),
        azp: vec![],
        max_ttl_secs: None,
        token_header: None,
        provision: Provision::Auto,
        verifier: ExternalVerifier::Jwks {
            url: url.into(),
            algs: vec![Algorithm::RS256, Algorithm::EdDSA],
        },
    }
}

fn token(key: &TestKey) -> String {
    sign_rs256_value(
        key,
        KID,
        &json!({
            "sub": "alice", "iss": EXTERNAL_ISSUER, "aud": EXTERNAL_AUDIENCE,
            "exp": now() + 300, "nbf": now() - 60, "iat": now(),
        }),
    )
}

/// The RSA key goes; only the Ed25519 key remains.
fn ed_only() -> serde_json::Value {
    json!({ "keys": [ed25519_jwk()] })
}

#[tokio::test]
async fn a_key_the_issuer_removed_stops_verifying_within_the_max_age() {
    let key = make_key();
    let iss = issuer(external_jwks(&key)).await;
    let cache = JwksCache::with_timing(iss.url.clone(), reqwest::Client::new(), quick());
    let ext = ext(&iss.url);
    let t = token(&key);

    assert!(
        external::verify_token(&t, &ext, &cache).await.is_ok(),
        "positive control: the token did not verify while its key was published"
    );

    // The issuer withdraws the key. Inside the max-age the cached set is still trusted (that is
    // what a cache is for), so this is not yet a failure...
    *iss.body.lock().unwrap() = ed_only();
    assert!(external::verify_token(&t, &ext, &cache).await.is_ok());

    // ...but once the max-age has passed the set is refetched and REPLACED, and the removed key is
    // gone. Before this change the token verified until the process restarted.
    tokio::time::sleep(MAX_AGE + Duration::from_millis(100)).await;
    assert!(
        external::verify_token(&t, &ext, &cache).await.is_err(),
        "a token signed by a key the issuer removed still verifies after the max-age"
    );
}

#[tokio::test]
async fn an_issuer_outage_is_survived_for_the_grace_period_and_no_longer() {
    let key = make_key();
    let iss = issuer(external_jwks(&key)).await;
    let cache = JwksCache::with_timing(iss.url.clone(), reqwest::Client::new(), quick());
    let ext = ext(&iss.url);
    let t = token(&key);
    assert!(external::verify_token(&t, &ext, &cache).await.is_ok());

    // The issuer goes down (and has removed the key, which we cannot learn while it is down).
    *iss.body.lock().unwrap() = ed_only();
    *iss.failing.lock().unwrap() = true;

    // Past the max-age but inside the grace: the failed refresh must not lock everyone out.
    tokio::time::sleep(MAX_AGE + Duration::from_millis(80)).await;
    assert!(
        external::verify_token(&t, &ext, &cache).await.is_ok(),
        "an issuer outage locked out a key we still held inside the grace period"
    );

    // Past the grace, still failing: trust in the held key ends. "Serve what we have" is not
    // allowed to mean "trust a possibly-removed key for ever".
    tokio::time::sleep(GRACE + Duration::from_millis(150)).await;
    assert!(
        external::verify_token(&t, &ext, &cache).await.is_err(),
        "a key was still trusted after the max-age plus the grace, with the issuer unreachable"
    );
}

#[tokio::test]
async fn the_issuers_cache_control_shortens_the_trust_window() {
    let key = make_key();
    let iss = issuer(external_jwks(&key)).await;
    // `no-store` means do not reuse: floored at the refetch throttle (zero here), so the very
    // next lookup after a removal must already refetch.
    *iss.cache_control.lock().unwrap() = Some("no-store".into());
    let cache = JwksCache::with_timing(
        iss.url.clone(),
        reqwest::Client::new(),
        Timing {
            default_max_age: Duration::from_secs(600),
            ..quick()
        },
    );
    let ext = ext(&iss.url);
    let t = token(&key);
    assert!(external::verify_token(&t, &ext, &cache).await.is_ok());

    *iss.body.lock().unwrap() = ed_only();
    assert!(
        external::verify_token(&t, &ext, &cache).await.is_err(),
        "no-store was ignored: a removed key still verified from the cache"
    );
    assert!(iss.hits.load(std::sync::atomic::Ordering::SeqCst) >= 2);
}
