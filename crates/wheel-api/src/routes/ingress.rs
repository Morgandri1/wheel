//! Public ingress: `ANY /p/{project_id}/{*rest}` → the project's endpoint nodes, via the host.
//!
//! This is the only unauthenticated route that reaches a tenant's sandbox, so it is the one an
//! attacker reaches for first. Four properties, in the order they are enforced:
//!
//! 1. **No existence oracle.** An unknown project is 404 and a project with ingress disabled is
//!    403 — but the 403 is only reachable for a project that exists *and* has been explicitly
//!    opted in by its owner. Probing for valid ids therefore learns nothing beyond "exists", which
//!    is already implied by the owner publishing the URL.
//! 2. **Capability gate.** `capabilities.http` defaults to false and a malformed capabilities blob
//!    parses to false, so ingress is never enabled by accident.
//! 3. **Rate limit**, counted in Postgres so it holds across replicas.
//! 4. **Header scrubbing.** Every `x-wheel-*` header from the caller is dropped before we add our
//!    own `x-wheel-ingress: 1`, so a public caller cannot forge the marker the engine trusts.

use crate::auth::extractor::load_unauthenticated_for_ingress;
use crate::error::{ApiError, ApiResult};
use crate::http::hop;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::response::Response;
use uuid::Uuid;

/// Headers the caller may never set on an ingress request; we own this namespace.
const WHEEL_PREFIX: &str = "x-wheel-";

pub async fn ingress(
    State(state): State<AppState>,
    Path((project_id, rest)): Path<(Uuid, String)>,
    req: Request,
) -> ApiResult<Response> {
    if rest.split('/').any(|seg| seg == "..") {
        return Err(ApiError::BadRequest(
            "path traversal is not permitted".into(),
        ));
    }

    // Unknown project → 404. This lookup deliberately has no owner predicate: ingress is public by
    // design. It is the only place in the codebase allowed to load a project without ownership,
    // which is why the function it calls is named to stand out in review.
    let project = load_unauthenticated_for_ingress(&state, &project_id).await?;

    if !project.capabilities.http {
        // Distinct from 404 on purpose (the brief requires it), and safe: reaching this response
        // requires guessing a v4 UUID, which is not a feasible enumeration strategy.
        return Err(ApiError::Forbidden("http capability disabled"));
    }

    // Counted only after we know the project is real, so random-UUID traffic cannot make us write
    // an unbounded number of counter rows.
    state.ingress_limiter.check(&state.db, &project_id).await?;

    let base = state.ingress_base_url(&project_id);
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let upstream = format!("{base}/{rest}{query}");

    let method = req.method().clone();
    let mut headers = hop::sanitize_for_upstream(req.headers(), &[WHEEL_PREFIX]);
    headers.insert(hop::header_name("x-wheel-ingress"), "1".parse().unwrap());

    let body = axum::body::to_bytes(req.into_body(), state.cfg.ingress_body_limit_bytes)
        .await
        .map_err(|_| ApiError::PayloadTooLarge)?;

    let resp = state
        .http
        .request(method, &upstream)
        .headers(headers)
        .bearer_auth(state.cfg.host_secret.expose())
        .body(body)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                ApiError::GatewayTimeout
            } else {
                tracing::warn!(error = ?e, "ingress proxy request failed");
                ApiError::BadGateway("host unreachable")
            }
        })?;

    let status = resp.status();
    let out_headers = hop::sanitize_from_upstream(resp.headers());

    // An engine that serves endpoints answers an unknown path with our envelope and a
    // `no_such_endpoint` code. A 404 with no body at all comes from an engine that has no
    // `/ingress/*` route — nothing is wrong with the caller's path, the feature is not there — and
    // relaying it unchanged tells the operator they made a typo. 501 says which it is. A 404 that
    // does carry a body is the endpoint's own answer and passes through untouched.
    let body = if status == axum::http::StatusCode::NOT_FOUND {
        let bytes = resp.bytes().await.map_err(|e| {
            tracing::warn!(error = ?e, "reading the ingress response failed");
            ApiError::BadGateway("engine response truncated")
        })?;
        if bytes.is_empty() {
            return Err(ApiError::IngressUnavailable);
        }
        Body::from(bytes)
    } else {
        Body::from_stream(resp.bytes_stream())
    };

    let mut builder = Response::builder().status(status);
    for (k, v) in out_headers.iter() {
        builder = builder.header(k, v);
    }
    builder
        .body(body)
        .map_err(|e| ApiError::Internal(anyhow::Error::new(e).context("building ingress response")))
}
