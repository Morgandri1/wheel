//! JWKS cache for Clerk's RS256 signing keys.
//!
//! Two properties matter here:
//!   * **Key rotation must not cause an outage** — an unknown `kid` triggers a refetch.
//!   * **Refetching must not become a DoS amplifier** — an attacker can mint tokens with random
//!     `kid`s all day; if each one caused an outbound fetch, our auth path would become a
//!     traffic pump aimed at Clerk (and a way to stall our own request handlers). So refreshes
//!     are throttled to at most once per minute, and a throttled miss is simply a rejection.

use anyhow::{Context, Result};
use jsonwebtoken::jwk::{AlgorithmParameters, JwkSet};
use jsonwebtoken::DecodingKey;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

struct Inner {
    keys: HashMap<String, Arc<DecodingKey>>,
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
    pub async fn key_for(&self, kid: &str) -> Option<Arc<DecodingKey>> {
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

    async fn fetch(&self) -> Result<HashMap<String, Arc<DecodingKey>>> {
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

        let mut out = HashMap::new();
        for jwk in &set.keys {
            // Only RSA keys are admissible. If Clerk ever served a symmetric (`oct`) key, importing
            // it here would hand an attacker an HMAC key that our verifier trusts — the algorithm
            // confusion attack, delivered by the key set itself. Skip anything that isn't RSA.
            let AlgorithmParameters::RSA(_) = &jwk.algorithm else {
                tracing::warn!("skipping non-RSA key in JWKS");
                continue;
            };
            let Some(kid) = jwk.common.key_id.clone() else {
                tracing::warn!("skipping JWKS key with no kid");
                continue;
            };
            match DecodingKey::from_jwk(jwk) {
                Ok(k) => {
                    out.insert(kid, Arc::new(k));
                }
                Err(e) => tracing::warn!(error = ?e, "skipping unusable JWKS key"),
            }
        }
        anyhow::ensure!(!out.is_empty(), "JWKS contained no usable RSA keys");
        tracing::info!(count = out.len(), "loaded JWKS signing keys");
        Ok(out)
    }
}
