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

/// How often a refetch may be attempted at all, hit or miss. The DoS-amplification bound: an
/// attacker minting tokens with random `kid`s cannot make this cache pump requests at the issuer.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// How long a fetched key set is trusted when the issuer says nothing about it.
const DEFAULT_MAX_AGE: Duration = Duration::from_secs(600);

/// The most an issuer's `Cache-Control` may lengthen that to. A key an issuer has REMOVED — because
/// it was compromised, say — keeps verifying until this runs out, so an issuer that advertises a
/// day is not allowed to make Wheel hold it for one.
const MAX_AGE_CEILING: Duration = Duration::from_secs(3600);

/// How much longer an already-held key set may be used when a refresh FAILS. Availability against
/// an issuer outage, bounded: without a bound "serve what we have" is "trust a removed key for ever".
const STALE_GRACE: Duration = Duration::from_secs(3600);

/// A resolved signing key and the one algorithm it may verify.
#[derive(Clone)]
pub struct KeyEntry {
    pub key: Arc<DecodingKey>,
    pub alg: Algorithm,
}

struct Inner {
    keys: HashMap<String, KeyEntry>,
    /// When a refetch was last ATTEMPTED — set before the request, so failures throttle too.
    last_refresh: Option<Instant>,
    /// When `keys` was last fetched successfully, and for how long that answer is good.
    fetched: Option<(Instant, Duration)>,
}

/// The three time bounds. Separate so a test can shrink them; production uses [`Timing::default`].
#[derive(Clone, Copy)]
pub struct Timing {
    /// Minimum gap between refetch attempts.
    pub min_refresh: Duration,
    /// Trust window for a fetched set when the issuer names none.
    pub default_max_age: Duration,
    /// Extra time a held set may be served when a refresh fails.
    pub stale_grace: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            min_refresh: MIN_REFRESH_INTERVAL,
            default_max_age: DEFAULT_MAX_AGE,
            stale_grace: STALE_GRACE,
        }
    }
}

pub struct JwksCache {
    url: String,
    http: reqwest::Client,
    timing: Timing,
    inner: RwLock<Inner>,
}

impl JwksCache {
    pub fn new(url: String, http: reqwest::Client) -> Self {
        Self::with_timing(url, http, Timing::default())
    }

    pub fn with_timing(url: String, http: reqwest::Client, timing: Timing) -> Self {
        Self {
            url,
            http,
            timing,
            inner: RwLock::new(Inner {
                keys: HashMap::new(),
                last_refresh: None,
                fetched: None,
            }),
        }
    }

    /// Look up a signing key by `kid`.
    ///
    /// A key is served only while the set it came from is still trusted: fresh (younger than its
    /// max-age), or — when a refresh has just failed — stale by no more than the grace period.
    /// Otherwise the set is refetched, and the refetched set REPLACES the old one, so a key the
    /// issuer has removed stops verifying within the max-age rather than surviving until restart.
    /// A miss on an unknown `kid` refetches too (key rotation), and every refetch attempt, hit or
    /// miss, is throttled to one per `min_refresh`.
    pub async fn key_for(&self, kid: &str) -> Option<KeyEntry> {
        {
            let inner = self.inner.read().await;
            if let (Some(k), true) = (inner.keys.get(kid), inner.is_fresh()) {
                return Some(k.clone());
            }
        }

        {
            // Re-check under the write lock: several requests can miss concurrently, and only the
            // first should perform the fetch.
            let mut inner = self.inner.write().await;
            if let (Some(k), true) = (inner.keys.get(kid), inner.is_fresh()) {
                return Some(k.clone());
            }
            let due = inner
                .last_refresh
                .map(|t| t.elapsed() >= self.timing.min_refresh)
                .unwrap_or(true);
            if !due {
                tracing::debug!(kid, "kid not servable from a fresh set, refresh throttled");
                return inner.usable_stale(self.timing.stale_grace, kid);
            }
            inner.last_refresh = Some(Instant::now());
        }

        match self.fetch().await {
            Ok((fresh, max_age)) => {
                let mut inner = self.inner.write().await;
                inner.keys = fresh;
                inner.fetched = Some((Instant::now(), max_age));
                inner.keys.get(kid).cloned()
            }
            Err(e) => {
                tracing::error!(error = ?e, "JWKS refresh failed");
                // Serve the set we hold, but only inside the grace period: an issuer outage must
                // not lock everyone out, and must not extend trust in a key indefinitely either.
                self.inner
                    .read()
                    .await
                    .usable_stale(self.timing.stale_grace, kid)
            }
        }
    }

    /// Warm the cache at boot so the first real request isn't paying for the fetch.
    pub async fn prime(&self) -> Result<()> {
        let (fresh, max_age) = self.fetch().await?;
        let mut inner = self.inner.write().await;
        inner.keys = fresh;
        inner.last_refresh = Some(Instant::now());
        inner.fetched = Some((Instant::now(), max_age));
        Ok(())
    }

    async fn fetch(&self) -> Result<(HashMap<String, KeyEntry>, Duration)> {
        let resp = self
            .http
            .get(&self.url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("fetching JWKS")?
            .error_for_status()
            .context("JWKS endpoint returned an error status")?;
        let max_age = resp
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .and_then(cache_control_max_age)
            .unwrap_or(self.timing.default_max_age)
            .clamp(
                self.timing.min_refresh,
                MAX_AGE_CEILING.max(self.timing.min_refresh),
            );
        let set: JwkSet = resp.json().await.context("parsing JWKS")?;
        let out = admissible_keys(&set);
        anyhow::ensure!(!out.is_empty(), "JWKS contained no usable signing keys");
        tracing::info!(count = out.len(), ?max_age, "loaded JWKS signing keys");
        Ok((out, max_age))
    }
}

impl Inner {
    /// Is the held set still inside the max-age its issuer (or our default) gave it?
    fn is_fresh(&self) -> bool {
        self.fetched
            .map(|(at, max_age)| at.elapsed() < max_age)
            .unwrap_or(false)
    }

    /// `kid`'s key from a set that is past its max-age, but only within the grace period.
    fn usable_stale(&self, grace: Duration, kid: &str) -> Option<KeyEntry> {
        let (at, max_age) = self.fetched?;
        (at.elapsed() < max_age + grace)
            .then(|| self.keys.get(kid).cloned())
            .flatten()
    }
}

/// `max-age` from a `Cache-Control` value. `no-store` and `no-cache` mean "do not reuse without
/// asking", which is a zero here (the caller floors it at the refetch throttle).
pub(crate) fn cache_control_max_age(value: &str) -> Option<Duration> {
    let mut max_age = None;
    for directive in value.split(',').map(|d| d.trim().to_ascii_lowercase()) {
        if directive == "no-store" || directive == "no-cache" {
            return Some(Duration::ZERO);
        }
        if let Some(n) = directive.strip_prefix("max-age=") {
            max_age = n
                .trim_matches('"')
                .parse::<u64>()
                .ok()
                .map(Duration::from_secs);
        }
    }
    max_age
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

    #[test]
    fn cache_control_max_age_is_read_and_no_store_is_zero() {
        use std::time::Duration;
        assert_eq!(
            cache_control_max_age("public, max-age=120"),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            cache_control_max_age("MAX-AGE=30, must-revalidate"),
            Some(Duration::from_secs(30))
        );
        assert_eq!(cache_control_max_age("no-store"), Some(Duration::ZERO));
        assert_eq!(
            cache_control_max_age("no-cache, max-age=600"),
            Some(Duration::ZERO)
        );
        assert_eq!(cache_control_max_age("public"), None);
        assert_eq!(cache_control_max_age("max-age=abc"), None);
    }
}
