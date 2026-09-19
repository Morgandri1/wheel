// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! JWKS cache for an identity provider's public signing keys.
//!
//! Three properties matter here:
//!   * **Key rotation must not cause an outage** — an unknown `kid` triggers a refetch.
//!   * **Refetching must not become a DoS amplifier** — an attacker can mint tokens with random
//!     `kid`s all day; if each one caused an outbound fetch, our auth path would become a traffic
//!     pump aimed at the provider (and a way to stall our own request handlers). So refreshes are
//!     throttled to at most once per minute, and a throttled miss is simply a rejection.
//!   * **A key carries its own algorithm.** This is the one that is new, and it is the reason this
//!     cache stores [`KeyEntry`] rather than a bare `DecodingKey`. A verifier that picks its
//!     algorithm from the token's `alg` header lets the attacker choose which verifier runs; a
//!     verifier that takes the algorithm from the key it resolved does not. The key set is the
//!     authority on what a key is for, because it is the thing we fetched over TLS from the
//!     provider rather than the thing the caller handed us.

use anyhow::{Context, Result};
use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, JwkSet};
use jsonwebtoken::{Algorithm, DecodingKey};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// A resolved signing key and the one algorithm it may verify.
#[derive(Clone)]
pub struct KeyEntry {
    pub key: Arc<DecodingKey>,
    pub alg: Algorithm,
}

struct Inner {
    keys: HashMap<String, KeyEntry>,
    last_refresh: Option<Instant>,
}

pub struct JwksCache {
    url: String,
    http: reqwest::Client,
    inner: RwLock<Inner>,
}

impl JwksCache {
    pub fn new(url: String, http: reqwest::Client) -> Self {
        Self {
            url,
            http,
            inner: RwLock::new(Inner {
                keys: HashMap::new(),
                last_refresh: None,
            }),
        }
    }

    /// Look up a signing key by `kid`, refreshing at most once per minute on a miss.
    pub async fn key_for(&self, kid: &str) -> Option<KeyEntry> {
        if let Some(k) = self.inner.read().await.keys.get(kid).cloned() {
            return Some(k);
        }

        {
            // Re-check under the write lock: several requests can miss concurrently, and only the
            // first should perform the fetch.
            let mut inner = self.inner.write().await;
            if let Some(k) = inner.keys.get(kid).cloned() {
                return Some(k);
            }
            let due = inner
                .last_refresh
                .map(|t| t.elapsed() >= MIN_REFRESH_INTERVAL)
                .unwrap_or(true);
            if !due {
                tracing::debug!(kid, "unknown kid, refresh throttled");
                return None;
            }
            inner.last_refresh = Some(Instant::now());
        }

        match self.fetch().await {
            Ok(fresh) => {
                let mut inner = self.inner.write().await;
                inner.keys = fresh;
                inner.keys.get(kid).cloned()
            }
            Err(e) => {
                // Keep serving with the keys we already have rather than failing every request.
                tracing::error!(error = ?e, "JWKS refresh failed");
                None
            }
        }
    }

    /// Warm the cache at boot so the first real request isn't paying for the fetch.
    pub async fn prime(&self) -> Result<()> {
        let fresh = self.fetch().await?;
        let mut inner = self.inner.write().await;
        inner.keys = fresh;
        inner.last_refresh = Some(Instant::now());
        Ok(())
    }

    async fn fetch(&self) -> Result<HashMap<String, KeyEntry>> {
        let set: JwkSet = self
            .http
            .get(&self.url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("fetching JWKS")?
            .error_for_status()
            .context("JWKS endpoint returned an error status")?
            .json()
            .await
            .context("parsing JWKS")?;
        let out = admissible_keys(&set);
        anyhow::ensure!(!out.is_empty(), "JWKS contained no usable signing keys");
        tracing::info!(count = out.len(), "loaded JWKS signing keys");
        Ok(out)
    }
}

/// The algorithm a JWK is for, or `None` if we refuse to hold it at all.
///
/// This is where algorithm confusion is stopped, one level before any token is seen:
///
///   * `oct` — a symmetric key. Importing one would hand an attacker an HMAC key our verifier
///     trusts, which is the confusion attack delivered by the key set itself. Never admitted.
///   * `OKP` on a curve that is not Ed25519 — X25519 is a key-*agreement* key. Treating one as a
///     signature key is not a mistake to tolerate, it is the shape of a downgrade.
///   * `EC` — no deployment needs it yet, and a key type we do not verify against is a key type we
///     have no tests for. Refused until someone asks, rather than admitted speculatively.
fn algorithm_of(jwk: &jsonwebtoken::jwk::Jwk) -> Option<Algorithm> {
    match &jwk.algorithm {
        AlgorithmParameters::RSA(_) => Some(Algorithm::RS256),
        AlgorithmParameters::OctetKeyPair(p) if p.curve == EllipticCurve::Ed25519 => {
            Some(Algorithm::EdDSA)
        }
        _ => None,
    }
}

/// Every key in the set we are willing to verify against, by `kid`.
///
/// Separated from [`JwksCache::fetch`] so the admission rules can be tested against a literal key
/// set without a server: this function is the security boundary, and the HTTP around it is not.
pub(crate) fn admissible_keys(set: &JwkSet) -> HashMap<String, KeyEntry> {
    let mut out = HashMap::new();
    for jwk in &set.keys {
        let Some(alg) = algorithm_of(jwk) else {
            tracing::warn!("skipping JWKS key of an unusable type");
            continue;
        };
        let Some(kid) = jwk.common.key_id.clone() else {
            tracing::warn!("skipping JWKS key with no kid");
            continue;
        };
        match DecodingKey::from_jwk(jwk) {
            Ok(key) => {
                out.insert(
                    kid,
                    KeyEntry {
                        key: Arc::new(key),
                        alg,
                    },
                );
            }
            Err(e) => tracing::warn!(error = ?e, "skipping unusable JWKS key"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(json: serde_json::Value) -> JwkSet {
        serde_json::from_value(json).expect("fixture is a valid JWKS")
    }

    /// A real RSA modulus/exponent pair; only its shape matters here.
    const RSA_N: &str = "sXchDaQebHnPiGvyDOAT4saGEUetSyo9MKLOoWFsueri23bOdgWp4Dy1Wl\
UzewbgBHod5pcM9H95GQRV3JDXboIRROSBigeC5yjU1hGzHHyXss8UDpre\
cbAYxknTcQkhslANGRUZmdTOQ5qTRsLAt6BTYuyvVRdhS8exSZEy_c4gs_\
7svlJJQ4H9_NxsiIoLwAEk7-Q3UXERGYw_75IDrGA84-lA_-Ct4eTlXHBI\
Y2EaV7t7LjJaynVJCpkv4LKjTTAumiGUIuQhrNhZLuF_RJLqHpM2kgWFLU\
7-VTdL1VbC2tejvcI2BlMkEpk1BzBZI0KQB0GaDWFLN-aEAw3vRw";

    #[test]
    fn an_rsa_key_is_admitted_as_rs256() {
        let keys = admissible_keys(&set(serde_json::json!({
            "keys": [{ "kty": "RSA", "kid": "r1", "n": RSA_N, "e": "AQAB" }]
        })));
        assert_eq!(keys["r1"].alg, Algorithm::RS256);
    }

    #[test]
    fn an_ed25519_key_is_admitted_as_eddsa() {
        let keys = admissible_keys(&set(serde_json::json!({
            "keys": [{
                "kty": "OKP", "kid": "e1", "crv": "Ed25519",
                "x": "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"
            }]
        })));
        assert_eq!(keys["e1"].alg, Algorithm::EdDSA);
    }

    /// The algorithm-confusion attack delivered by the key set itself: if we imported a symmetric
    /// key, a token signed with HMAC under a value the provider published would verify.
    #[test]
    fn a_symmetric_key_is_never_admitted() {
        let keys = admissible_keys(&set(serde_json::json!({
            "keys": [{ "kty": "oct", "kid": "h1", "k": "c2VjcmV0LWtleS1tYXRlcmlhbA" }]
        })));
        assert!(keys.is_empty(), "an oct key must never be held");
    }

    /// X25519 is for key agreement. Admitting it as a signing key is a downgrade, not a typo.
    #[test]
    fn an_okp_key_on_a_non_ed25519_curve_is_refused() {
        let keys = admissible_keys(&set(serde_json::json!({
            "keys": [{
                "kty": "OKP", "kid": "x1", "crv": "P-256",
                "x": "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"
            }]
        })));
        assert!(keys.is_empty(), "only Ed25519 is a signing curve here");
    }

    #[test]
    fn a_key_with_no_kid_is_skipped_rather_than_guessed_at() {
        let keys = admissible_keys(&set(serde_json::json!({
            "keys": [{ "kty": "RSA", "n": RSA_N, "e": "AQAB" }]
        })));
        assert!(keys.is_empty());
    }

    /// One unusable key must not discard the usable ones beside it: a provider mid-rotation
    /// publishing a type we do not hold would otherwise take the deployment down.
    #[test]
    fn a_mixed_set_keeps_the_keys_it_can_use() {
        let keys = admissible_keys(&set(serde_json::json!({
            "keys": [
                { "kty": "oct", "kid": "h1", "k": "c2VjcmV0" },
                { "kty": "RSA", "kid": "r1", "n": RSA_N, "e": "AQAB" }
            ]
        })));
        assert_eq!(keys.len(), 1);
        assert_eq!(keys["r1"].alg, Algorithm::RS256);
    }
}
