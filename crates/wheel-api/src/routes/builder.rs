// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! `POST /v1/projects/{id}/builder/turns` — the Workflow Builder's conversation, streamed.
//!
//! A dedicated route rather than the generic engine proxy, for one measured reason: the shared
//! client carries a whole-request timeout (`proxy_timeout_secs`, 30s by default) that would cut a
//! builder turn mid-answer. Everything else is the proxy's discipline — `ProjectScope` proves
//! ownership before a byte is forwarded, the host bearer is attached here and never travels back,
//! and the upstream URL is built from a `Uuid` this API loaded from its own database.
//!
//! The body is passed through as it arrives, so the SSE frames reach the caller as events rather
//! than as one late response.

use crate::auth::ProjectScope;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::State;
use axum::response::Response;

/// A turn is bounded engine-side (240s); this is that bound plus room for the hop itself, so the
/// engine's own `timeout` frame is what a caller sees rather than a severed connection.
const BUILDER_TIMEOUT_SECS: u64 = 300;

/// The engine's builder route carries these back verbatim. `content-type` decides whether the
/// caller sees a stream or a refusal, and the two buffering hints keep a proxy from holding the
/// stream until it ends.
const STREAM_HEADERS: [(&str, &str); 2] = [
    ("cache-control", "no-cache, no-transform"),
    ("x-accel-buffering", "no"),
];

pub async fn turns(
    State(state): State<AppState>,
    scope: ProjectScope,
    body: axum::body::Bytes,
) -> ApiResult<Response> {
    if body.len() > state.cfg.ingress_body_limit_bytes {
        return Err(ApiError::PayloadTooLarge);
    }
    let upstream = format!(
        "{}/v1/builder/turns",
        state.engine_base_url(&scope.project.id)
    );

    let resp = state
        .http
        .post(&upstream)
        .header(
            "Authorization",
            format!("Bearer {}", state.cfg.host_secret.expose()),
        )
        .header("content-type", "application/json")
        .timeout(std::time::Duration::from_secs(BUILDER_TIMEOUT_SECS))
        .body(body)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                ApiError::GatewayTimeout
            } else {
                tracing::warn!(error = ?e, "builder turn could not reach the engine");
                ApiError::BadGateway("host unreachable")
            }
        })?;

    let status = resp.status();
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .cloned();

    let mut builder = Response::builder().status(status);
    if let Some(value) = content_type {
        builder = builder.header(axum::http::header::CONTENT_TYPE, value);
    }
    for (name, value) in STREAM_HEADERS {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from_stream(resp.bytes_stream()))
        .map_err(|e| {
            ApiError::Internal(anyhow::Error::new(e).context("building the builder stream"))
        })
}
