// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Authenticated proxy to a project's engine, via the host.
//!
//! Trust boundary notes:
//!   * The handler takes `ProjectScope`, so ownership is proven before a single byte is forwarded.
//!   * `WHEEL_HOST_SECRET` is attached here and never travels back to the client. The client's own
//!     credentials are stripped by `sanitize_for_upstream` — the host authenticates *us*, not the
//!     user, and relaying a user token downstream is how replay bugs start.
//!   * The upstream URL is the project's host base, built from a `Uuid` we loaded from our own
//!     database, with the caller's suffix appended by `wheel_core::proxy_path`. Concatenating the
//!     suffix instead let an encoded `..` be decoded again by the URL parser and climb into another
//!     project's route on the host, which answers with that project's engine secret.

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
use wheel_core::proxy_path::{self, ProxyPathError, Url};

/// Ceiling on the upstream WebSocket handshake. Generous for a healthy engine on the same private
/// network, and short enough that a stalled peer cannot hold the connection open indefinitely.
const WS_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub async fn engine_proxy(
    State(state): State<AppState>,
    scope: ProjectScope,
    Path((_id, rest)): Path<(uuid::Uuid, String)>,
    req: Request,
) -> ApiResult<Response> {
    refuse_ambiguous_path(&rest)?;
    let upstream = upstream_url(
        &state.engine_base_url(&scope.project.id),
        &rest,
        req.uri().query(),
    )?;

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

/// Refuse a caller's path suffix that a parser further down could read differently. Runs before
/// any lookup, so the answer is the same whichever project the path names.
pub(crate) fn refuse_ambiguous_path(rest: &str) -> ApiResult<()> {
    proxy_path::proxy_segments(rest)
        .map(|_| ())
        .map_err(|_| path_refused())
}

/// `base` with the caller's decoded suffix appended segment by segment and the raw query attached.
pub(crate) fn upstream_url(base: &str, rest: &str, query: Option<&str>) -> ApiResult<Url> {
    proxy_path::upstream_url(base, rest, query).map_err(|e| match e {
        ProxyPathError::BadBase => {
            ApiError::Internal(anyhow::anyhow!("the host base URL cannot carry a path"))
        }
        _ => path_refused(),
    })
}

fn path_refused() -> ApiError {
    ApiError::BadRequest("path traversal is not permitted".into())
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

/// `ANY /v1/projects/{id}/engine/v1/events` — the events WebSocket.
///
/// Registered ahead of the generic engine wildcard because it accepts a second, narrower form of
/// authentication: a single-use ticket in the query string, for browsers that cannot set headers
/// on a WebSocket handshake.
///
/// Header auth still works and is preferred for non-browser clients. The ticket path is strictly
/// additional, and it is *not* a weaker door: a ticket can only be minted by an authenticated
/// owner for one specific project, survives 30 seconds, and is consumed on first use.
pub async fn engine_events(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    req: Request,
) -> ApiResult<Response> {
    let (mut parts, body) = req.into_parts();
    let raw_query = parts.uri.query().unwrap_or("").to_string();

    match query_param(&raw_query, "ticket") {
        Some(ticket) => {
            // Redemption proves the caller owned this project when the ticket was minted, and
            // binds it to this project id specifically.
            crate::routes::ws_ticket::redeem(&state, &ticket, &id).await?;
        }
        None => {
            // No ticket: fall back to the ordinary header-authenticated path, which also proves
            // ownership. Either way we do not reach the engine without one of the two.
            ProjectScope::from_request_parts(&mut parts, &state).await?;
        }
    }

    // The ticket is deliberately dropped here rather than forwarded: it has already been consumed,
    // and passing credentials further down the chain is how replay bugs start.
    let forwarded = strip_query_param(&raw_query, "ticket");
    let upstream = upstream_url(
        &state.engine_base_url(&id),
        "v1/events",
        (!forwarded.is_empty()).then_some(forwarded.as_str()),
    )?;

    let req = Request::from_parts(parts, body);
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

/// Read one parameter out of a raw query string.
fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| urldecode(v))
    })
}

/// Everything except the named parameter, re-joined.
fn strip_query_param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter(|pair| pair.split_once('=').map(|(k, _)| k) != Some(key))
        .collect::<Vec<_>>()
        .join("&")
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 3;
                }
                Err(_) => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn forward_http(state: AppState, req: Request, upstream: Url) -> ApiResult<Response> {
    let method = req.method().clone();
    let headers = hop::sanitize_for_upstream(req.headers(), &[]);

    // Buffer the body against the configured cap. Streaming would be nicer, but an unbounded
    // stream from an authenticated client is still a memory-exhaustion vector across N replicas.
    let body = axum::body::to_bytes(req.into_body(), state.cfg.ingress_body_limit_bytes)
        .await
        .map_err(|_| ApiError::PayloadTooLarge)?;

    let resp = state
        .http
        .request(method, upstream)
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
    upstream: Url,
) -> ApiResult<Response> {
    let ws_url = proxy_path::websocket_url(upstream).ok_or_else(|| {
        ApiError::Internal(anyhow::anyhow!("the host base URL has no websocket scheme"))
    })?;

    let request = tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(ws_url.as_str())
        .header(
            "Authorization",
            format!("Bearer {}", state.cfg.host_secret.expose()),
        )
        // Handshake headers required by RFC 6455; tungstenite does not add these for a raw request.
        .header("Host", proxy_path::authority(&ws_url))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .map_err(|e| ApiError::Internal(anyhow::Error::new(e).context("building ws request")))?;

    // Bound the upstream handshake (ADVERSARY: unbounded connect on the bridge path).
    // A peer that completes the TCP connection and then simply never finishes the WebSocket
    // handshake would otherwise pin this task, and the client's connection with it, for as long as
    // it liked — one slow-loris connection per request, with no ceiling.
    let connect = tokio::time::timeout(
        WS_HANDSHAKE_TIMEOUT,
        tokio_tungstenite::connect_async(request),
    );
    let (upstream, _resp) = match connect.await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            tracing::warn!(error = ?e, "engine websocket connect failed");
            return Err(ApiError::BadGateway("engine websocket unreachable"));
        }
        Err(_elapsed) => {
            tracing::warn!("engine websocket handshake timed out");
            return Err(ApiError::GatewayTimeout);
        }
    };

    Ok(upgrade.on_upgrade(move |client| pump(client, upstream)))
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
