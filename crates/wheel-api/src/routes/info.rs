// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! `GET /v1/info`: API-layer capability discovery, mirroring `wheel-engine`'s `GET /v1/engine`.
//!
//! Some capabilities are the API's to report, not the engine's. Membership
//! (`crate::membership`, `routes::members`) is the first: it is enforced entirely in this crate's
//! own auth/policy layer before a request ever reaches a project's engine, so `wheel-engine` has no
//! route, config field or behaviour a test could hold a `"membership"` FEATURES id to. Advertising
//! it there would be a permanently-true string with no engine-side truth behind it — sdk's own
//! objection, reviewing this design. This is the discovery surface for capabilities like that
//! instead, so a client checks the layer that actually owns the capability rather than inferring
//! support from whether `Project.tier` happens to be present in a response.

use axum::Json;
use serde::Serialize;

/// Stable ids a client may test for before depending on a capability. Every id names a route this
/// build actually serves — `tests/info.rs` holds each one to a real request against it, the same
/// discipline `wheel-engine`'s own `FEATURES` list uses.
pub const FEATURES: &[&str] = &["membership"];

#[derive(Debug, Serialize)]
pub struct ApiInfo {
    /// The API crate version, compiled in.
    pub version: String,
    /// The public API's contract version, `v1`.
    pub api_version: String,
    /// Stable ids of the capabilities this deployment honours. Clients ignore ids they do not know.
    pub features: Vec<String>,
}

pub async fn info() -> Json<ApiInfo> {
    Json(ApiInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        api_version: "v1".into(),
        features: FEATURES.iter().map(|s| s.to_string()).collect(),
    })
}
