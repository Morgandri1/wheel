//! Authenticated proxy to a project's engine, via the host.
//!
//! Trust boundary notes:
//!   * The handler takes `ProjectScope`, so ownership is proven before a single byte is forwarded.
//!   * `WHEEL_HOST_SECRET` is attached here and never travels back to the client. The client's own
//!     credentials are stripped by `sanitize_for_upstream` — the host authenticates *us*, not the
//!     user, and relaying a user token downstream is how replay bugs start.
//!   * The upstream URL is built from a `Uuid` we loaded from our own database plus a path
//!     suffix that axum already percent-decoded and split, so the client cannot redirect the
//!     proxy at another project (or another host) by smuggling `../` or an absolute URL.

use crate::auth::ProjectScope;
use crate::error::{ApiError, ApiResult};
use crate::http::hop;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::ws::{Message as AxumMsg, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, Request, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as TungMsg;

pub async fn engine_proxy(
    State(state): State<AppState>,
    scope: ProjectScope,
    Path((_id, rest)): Path<(uuid::Uuid, String)>,
    req: Request,
) -> ApiResult<Response> {
    // Reject traversal outright rather than relying on the upstream to normalise it.
    if rest.split('/').any(|seg| seg == "..") {
        return Err(ApiError::BadRequest(
            "path traversal is not permitted".into(),
        ));
    }

    let base = state.engine_base_url(&scope.project.id);
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let upstream = format!("{base}/{rest}{query}");

    // axum 0.8 will not extract `Option<WebSocketUpgrade>` (that needs `OptionalFromRequestParts`,
    // which `WebSocketUpgrade` does not implement), so the upgrade is detected explicitly and the
    // extractor is run by hand only on that branch.
    if is_websocket_upgrade(req.headers()) {
        let (mut parts, _) = req.into_parts();
        let upgrade = WebSocketUpgrade::from_request_parts(&mut parts, &state)
            .await
            .map_err(|_| ApiError::BadRequest("malformed websocket upgrade".into()))?;
        bridge_websocket(state, upgrade, upstream).await
    } else {
        forward_http(state, req, upstream).await
    }
}

/// RFC 6455 handshake detection: `Upgrade: websocket` plus `Connection: Upgrade`, both
/// case-insensitive, and `Connection` may be a comma-separated list.
fn is_websocket_upgrade(headers: &axum::http::HeaderMap) -> bool {
    let upgrade_ok = headers
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));

    let connection_ok = headers
        .get(axum::http::header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
        });

    upgrade_ok && connection_ok
}

async fn forward_http(state: AppState, req: Request, upstream: String) -> ApiResult<Response> {
    let method = req.method().clone();
    let headers = hop::sanitize_for_upstream(req.headers(), &[]);

    // Buffer the body against the configured cap. Streaming would be nicer, but an unbounded
    // stream from an authenticated client is still a memory-exhaustion vector across N replicas.
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
                tracing::warn!(error = ?e, "host proxy request failed");
                ApiError::BadGateway("host unreachable")
            }
        })?;

    let status = resp.status();
    let out_headers = hop::sanitize_from_upstream(resp.headers());
    let stream = resp.bytes_stream();

    let mut builder = Response::builder().status(status);
    for (k, v) in out_headers.iter() {
        builder = builder.header(k, v);
    }
    builder
        .body(Body::from_stream(stream))
        .map_err(|e| ApiError::Internal(anyhow::Error::new(e).context("building proxy response")))
}

/// Bridge a client WebSocket to the engine's `/v1/events`, verbatim in both directions.
///
/// Frames are relayed without inspection or re-encoding. That matters for the `message` event in
/// particular: the contract requires it to pass through unmodified so the UI can correlate a
/// message row by its id, and re-serialising JSON here could reorder keys or alter the body.
async fn bridge_websocket(
    state: AppState,
    upgrade: WebSocketUpgrade,
    upstream_http: String,
) -> ApiResult<Response> {
    let ws_url = upstream_http
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);

    let request = tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(&ws_url)
        .header(
            "Authorization",
            format!("Bearer {}", state.cfg.host_secret.expose()),
        )
        // Handshake headers required by RFC 6455; tungstenite does not add these for a raw request.
        .header("Host", host_of(&ws_url).unwrap_or_default())
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .map_err(|e| ApiError::Internal(anyhow::Error::new(e).context("building ws request")))?;

    let (upstream, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| {
            tracing::warn!(error = ?e, "engine websocket connect failed");
            ApiError::BadGateway("engine websocket unreachable")
        })?;

    Ok(upgrade.on_upgrade(move |client| pump(client, upstream)))
}

fn host_of(url: &str) -> Option<String> {
    let after_scheme = url.split("://").nth(1)?;
    Some(after_scheme.split('/').next()?.to_string())
}

async fn pump(
    client: WebSocket,
    upstream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    let (mut client_tx, mut client_rx) = client.split();
    let (mut up_tx, mut up_rx) = upstream.split();

    // Either direction closing tears down both, so a half-open connection cannot leak a task.
    let to_upstream = async {
        while let Some(Ok(msg)) = client_rx.next().await {
            let out = match msg {
                AxumMsg::Text(t) => TungMsg::Text(t.as_str().into()),
                AxumMsg::Binary(b) => TungMsg::Binary(b),
                AxumMsg::Ping(p) => TungMsg::Ping(p),
                AxumMsg::Pong(p) => TungMsg::Pong(p),
                AxumMsg::Close(_) => break,
            };
            if up_tx.send(out).await.is_err() {
                break;
            }
        }
        let _ = up_tx.close().await;
    };

    let to_client = async {
        while let Some(Ok(msg)) = up_rx.next().await {
            let out = match msg {
                TungMsg::Text(t) => AxumMsg::Text(t.as_str().into()),
                TungMsg::Binary(b) => AxumMsg::Binary(b),
                TungMsg::Ping(p) => AxumMsg::Ping(p),
                TungMsg::Pong(p) => AxumMsg::Pong(p),
                TungMsg::Close(_) => break,
                TungMsg::Frame(_) => continue,
            };
            if client_tx.send(out).await.is_err() {
                break;
            }
        }
        let _ = client_tx.close().await;
    };

    tokio::select! {
        _ = to_upstream => {},
        _ = to_client => {},
    }
}
