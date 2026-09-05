//! Proxy from the host to a project's engine.
//!
//! This is where the engine secret is attached. The host is the only process that holds engine
//! secrets at runtime (§4b): the API stores them encrypted and hands them over on `PUT`, and they
//! never travel back up. The API's own bearer has already been checked by the middleware before
//! anything here runs.

use crate::HostState;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

/// `ANY /host/v1/projects/{id}/engine/{*rest}` → the engine's `/v1/{rest}`.
pub async fn engine(
    State(state): State<HostState>,
    Path((id, rest)): Path<(Uuid, String)>,
    req: Request,
) -> Response {
    forward(state, id, format!("v1/{rest}"), req).await
}

/// `ANY /host/v1/projects/{id}/ingress/{*rest}` → the engine's `/ingress/{rest}`.
pub async fn ingress(
    State(state): State<HostState>,
    Path((id, rest)): Path<(Uuid, String)>,
    req: Request,
) -> Response {
    forward(state, id, format!("ingress/{rest}"), req).await
}

async fn forward(state: HostState, id: Uuid, suffix: String, req: Request) -> Response {
    if suffix.split('/').any(|seg| seg == "..") {
        return (StatusCode::BAD_REQUEST, "path traversal is not permitted").into_response();
    }

    let Ok(Some(rec)) = state.store.get(&id).await else {
        return (StatusCode::NOT_FOUND, "no such project").into_response();
    };

    let base = state.sandbox.engine_base(&id);
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let upstream = format!("{base}/{suffix}{query}");

    let method = req.method().clone();
    let mut headers = req.headers().clone();
    // Strip the API's bearer: the engine authenticates the *host*, with the engine secret added
    // below. Forwarding the host secret to a tenant's engine would hand every tenant the key to
    // every other tenant's sandbox.
    headers.remove(axum::http::header::AUTHORIZATION);
    for h in ["connection", "keep-alive", "transfer-encoding", "upgrade", "te", "trailer", "host", "content-length"] {
        headers.remove(h);
    }

    let body = match axum::body::to_bytes(req.into_body(), 16 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response(),
    };

    let resp = match state
        .http
        .request(method, &upstream)
        .headers(headers)
        .bearer_auth(&rec.engine_secret)
        .body(body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(project = %id, error = ?e, "engine proxy failed");
            return (StatusCode::BAD_GATEWAY, "engine unreachable").into_response();
        }
    };

    let status = resp.status();
    let mut builder = Response::builder().status(status);
    for (k, v) in resp.headers().iter() {
        let n = k.as_str().to_ascii_lowercase();
        if matches!(
            n.as_str(),
            "connection" | "keep-alive" | "transfer-encoding" | "upgrade" | "te" | "trailer" | "content-length"
        ) {
            continue;
        }
        builder = builder.header(k, v);
    }
    builder
        .body(Body::from_stream(resp.bytes_stream()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
