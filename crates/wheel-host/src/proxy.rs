//! Proxy from the host to a project's engine.
//!
//! This is where the engine secret is attached. The host is the only process that holds engine
//! secrets at runtime (§4b): the API stores them encrypted and hands them over on `PUT`, and they
//! never travel back up. The API's own bearer has already been checked by the middleware before
//! anything here runs.

use crate::HostState;
use axum::body::Body;
use axum::extract::ws::{Message as AxumMsg, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as TungMsg;
use uuid::Uuid;

/// Ceiling on the upstream WebSocket handshake, so a stalled engine cannot hold a bridge
/// half-open indefinitely.
const WS_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// `ANY /host/v1/projects/{id}/engine/{*rest}` → the engine's `/{rest}`.
///
/// The suffix is forwarded verbatim. Callers address the control plane as
/// `/v1/projects/<id>/engine/v1/board`, so `rest` already carries the engine's own `v1/` prefix;
/// adding another here produced `/v1/v1/board` and a 404 from the engine.
pub async fn engine(
    State(state): State<HostState>,
    Path((id, rest)): Path<(Uuid, String)>,
    req: Request,
) -> Response {
    forward(state, id, rest, req).await
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
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let upstream = format!("{base}/{suffix}{query}");

    // The events stream arrives here as a WebSocket upgrade rather than a normal request.
    if is_websocket_upgrade(req.headers()) {
        return bridge_ws(state, rec.engine_secret, upstream, req).await;
    }

    let method = req.method().clone();
    let mut headers = req.headers().clone();
    // Strip the API's bearer: the engine authenticates the *host*, with the engine secret added
    // below. Forwarding the host secret to a tenant's engine would hand every tenant the key to
    // every other tenant's sandbox.
    headers.remove(axum::http::header::AUTHORIZATION);
    for h in [
        "connection",
        "keep-alive",
        "transfer-encoding",
        "upgrade",
        "te",
        "trailer",
        "host",
        "content-length",
    ] {
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
            "connection"
                | "keep-alive"
                | "transfer-encoding"
                | "upgrade"
                | "te"
                | "trailer"
                | "content-length"
        ) {
            continue;
        }
        builder = builder.header(k, v);
    }
    builder
        .body(Body::from_stream(resp.bytes_stream()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn is_websocket_upgrade(headers: &axum::http::HeaderMap) -> bool {
    let upgrade = headers
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let connection = headers
        .get(axum::http::header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
        });
    upgrade && connection
}

/// Bridge the API's WebSocket to the engine's, relaying frames verbatim.
///
/// Frames are not inspected or re-encoded. The `message` event in particular must reach the UI
/// byte-for-byte so a row can be correlated by its id; re-serialising JSON here could reorder keys
/// or alter a body that the engine went to some trouble to deliver exactly.
async fn bridge_ws(
    state: HostState,
    engine_secret: String,
    upstream_http: String,
    req: Request,
) -> Response {
    let ws_url = upstream_http
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);

    let host_hdr = ws_url
        .split("://")
        .nth(1)
        .and_then(|s| s.split('/').next())
        .unwrap_or_default()
        .to_string();

    let upstream_req = match tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(&ws_url)
        .header("Authorization", format!("Bearer {engine_secret}"))
        .header("Host", host_hdr)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = ?e, "building engine ws request failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Connect upstream *before* accepting the client upgrade, so a dead engine surfaces as a clean
    // 502 rather than a WebSocket that opens and immediately closes.
    let (engine_ws, _) = match tokio::time::timeout(
        WS_HANDSHAKE_TIMEOUT,
        tokio_tungstenite::connect_async(upstream_req),
    )
    .await
    {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            tracing::warn!(error = ?e, "engine websocket connect failed");
            return (StatusCode::BAD_GATEWAY, "engine websocket unreachable").into_response();
        }
        Err(_elapsed) => {
            // An engine that accepts the socket and then stalls must not pin this task.
            tracing::warn!("engine websocket handshake timed out");
            return (
                StatusCode::GATEWAY_TIMEOUT,
                "engine websocket handshake timed out",
            )
                .into_response();
        }
    };

    let (mut parts, _) = req.into_parts();
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };

    upgrade.on_upgrade(move |client| pump(client, engine_ws))
}

async fn pump(
    client: WebSocket,
    engine: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    let (mut client_tx, mut client_rx) = client.split();
    let (mut eng_tx, mut eng_rx) = engine.split();

    let to_engine = async {
        while let Some(Ok(msg)) = client_rx.next().await {
            let out = match msg {
                AxumMsg::Text(t) => TungMsg::Text(t.as_str().into()),
                AxumMsg::Binary(b) => TungMsg::Binary(b),
                AxumMsg::Ping(p) => TungMsg::Ping(p),
                AxumMsg::Pong(p) => TungMsg::Pong(p),
                AxumMsg::Close(_) => break,
            };
            if eng_tx.send(out).await.is_err() {
                break;
            }
        }
        let _ = eng_tx.close().await;
    };

    let to_client = async {
        while let Some(Ok(msg)) = eng_rx.next().await {
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

    // Either side closing tears down both, so a half-open bridge cannot leak a task.
    tokio::select! {
        _ = to_engine => {},
        _ = to_client => {},
    }
}
