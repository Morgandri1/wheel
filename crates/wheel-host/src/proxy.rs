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

/// Uniform error body, matching the shape the API and the engine both use:
/// `{"error":{"code","message"}}`.
///
/// These responses are relayed to the client verbatim by the API's proxy, so a bare string here
/// surfaces to the browser as an untyped body that no client can branch on. Every failure that can
/// escape this module therefore has to carry the envelope.
fn err(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        axum::Json(serde_json::json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

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
        return err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "Path traversal is not permitted.",
        );
    }

    let Ok(Some(rec)) = state.store.get(&id).await else {
        return err(
            StatusCode::NOT_FOUND,
            "not_found",
            "The requested resource does not exist.",
        );
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
        Err(_) => {
            return err(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "Request body exceeds the maximum allowed size.",
            )
        }
    };

    // In process mode the engine has no TCP endpoint at all — `engine_base` names a unix socket,
    // which reqwest cannot dial. Route those over the socket instead of failing to parse a URL.
    // Note this reads the socket from `base`, not from `upstream`: `upstream` already has the
    // request path appended, so stripping the scheme off it would yield "<socket>/v1/board" and
    // try to connect to a path that does not exist.
    if let Some(socket) = base.strip_prefix("unix://") {
        return forward_over_socket(
            socket,
            &suffix,
            &query,
            method,
            headers,
            body,
            &rec.engine_secret,
            id,
        )
        .await;
    }

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
            return err(
                StatusCode::BAD_GATEWAY,
                "engine_unreachable",
                "The project engine is not reachable.",
            );
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
        .unwrap_or_else(|_| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "An unexpected error occurred.",
            )
        })
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
///
/// The engine socket, whichever transport reached it.
///
/// Both arms carry a full-duplex WebSocket and differ only in what is underneath. Keeping them in
/// one enum means the frame pump stays a single implementation, so the TCP and unix paths cannot
/// drift into relaying frames differently.
enum WsStream {
    // Boxed: the TLS-capable TCP stream is far larger than the unix one, and an unboxed enum would
    // pay that size on every connection including the unix ones we actually use in production.
    Tcp(
        Box<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ),
    Unix(Box<tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>>),
}

async fn bridge_ws(
    state: HostState,
    engine_secret: String,
    upstream_http: String,
    req: Request,
) -> Response {
    // In process mode the engine has no TCP endpoint at all: `upstream_http` names a unix socket,
    // and the http->ws rewrite below would leave a `unix://` URI that no WebSocket client can dial.
    // That is exactly what made the events socket 502 in production while ordinary HTTP over the
    // same socket worked — the backend looked healthy and only the live board was dead.
    let unix_socket = upstream_http
        .strip_prefix("unix://")
        .map(|rest| match rest.find("/v1/") {
            Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
            None => (rest.to_string(), "/".to_string()),
        });

    let ws_url = match &unix_socket {
        // A unix socket has no authority, but the handshake still needs a syntactically valid URI
        // and a Host header, so this uses a placeholder that never resolves.
        Some((_, path)) => format!("ws://engine{path}"),
        None => upstream_http
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1),
    };

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
    let connect = async {
        match &unix_socket {
            Some((sock, _)) => {
                let stream = tokio::net::UnixStream::connect(sock)
                    .await
                    .map_err(tokio_tungstenite::tungstenite::Error::Io)?;
                tokio_tungstenite::client_async(upstream_req, stream)
                    .await
                    .map(|(ws, resp)| (WsStream::Unix(Box::new(ws)), resp))
            }
            None => tokio_tungstenite::connect_async(upstream_req)
                .await
                .map(|(ws, resp)| (WsStream::Tcp(Box::new(ws)), resp)),
        }
    };

    let (engine_ws, _) = match tokio::time::timeout(WS_HANDSHAKE_TIMEOUT, connect).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            tracing::warn!(error = ?e, "engine websocket connect failed");
            return err(
                StatusCode::BAD_GATEWAY,
                "engine_unreachable",
                "The project engine websocket is not reachable.",
            );
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

    upgrade.on_upgrade(move |client| async move {
        match engine_ws {
            WsStream::Tcp(ws) => pump(client, *ws).await,
            WsStream::Unix(ws) => pump(client, *ws).await,
        }
    })
}

/// Generic over the engine transport so TCP and unix sockets share one implementation.
async fn pump<S>(client: WebSocket, engine: tokio_tungstenite::WebSocketStream<S>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
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

/// Speak HTTP/1.1 to an engine listening on a unix socket.
///
/// The socket is mode 0600 and owned by the project uid, inside a 0700 directory — SDK sets that
/// explicitly after bind rather than inheriting a umask, because under `umask 000` the inherited
/// mode was 0777 and on a shared kernel that is reachable by every tenant. The host runs as root
/// and so passes the permission check without anything being widened; if this ever starts failing,
/// the fix is to run the proxy as the project uid, never to loosen the mode.
#[allow(clippy::too_many_arguments)]
async fn forward_over_socket(
    socket: &str,
    suffix: &str,
    query: &str,
    method: axum::http::Method,
    headers: axum::http::HeaderMap,
    body: bytes::Bytes,
    engine_secret: &str,
    id: Uuid,
) -> Response {
    use http_body_util::BodyExt;
    use hyper_util::rt::TokioIo;

    let stream = match tokio::net::UnixStream::connect(socket).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(project = %id, error = ?e, socket, "engine socket unreachable");
            return err(
                StatusCode::BAD_GATEWAY,
                "engine_unreachable",
                "The project engine is not reachable.",
            );
        }
    };

    let (mut sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await
    {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!(project = %id, error = ?e, "engine handshake failed");
            return err(
                StatusCode::BAD_GATEWAY,
                "engine_unreachable",
                "The project engine is not reachable.",
            );
        }
    };
    // The connection task drives the socket; dropping it would stall the request.
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = hyper::Request::builder()
        .method(method)
        .uri(format!("/{suffix}{query}"))
        // A unix socket has no authority, but HTTP/1.1 still requires a Host header.
        .header("host", "engine")
        .header("authorization", format!("Bearer {engine_secret}"));
    for (k, v) in headers.iter() {
        builder = builder.header(k, v);
    }

    let request = match builder.body(http_body_util::Full::new(body)) {
        Ok(r) => r,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "The request could not be forwarded.",
            )
        }
    };

    let upstream = match sender.send_request(request).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(project = %id, error = ?e, "engine request failed");
            return err(
                StatusCode::BAD_GATEWAY,
                "engine_unreachable",
                "The project engine is not reachable.",
            );
        }
    };

    let status = upstream.status();
    let (parts, incoming) = upstream.into_parts();
    let collected = match incoming.collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return err(
                StatusCode::BAD_GATEWAY,
                "engine_unreachable",
                "The project engine closed the connection.",
            )
        }
    };

    let mut out = Response::builder().status(status);
    for (k, v) in parts.headers.iter() {
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
        out = out.header(k, v);
    }
    out.body(Body::from(collected)).unwrap_or_else(|_| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "An unexpected error occurred.",
        )
    })
}

#[cfg(test)]
mod upgrade_detection_tests {
    use super::is_websocket_upgrade;
    use axum::http::{HeaderMap, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.append(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        m
    }

    /// Getting this wrong is silent in both directions: miss an upgrade and the events socket is
    /// answered as a normal request that never streams; see one where there is none and an
    /// ordinary call is hijacked into a handshake.
    #[test]
    fn a_real_handshake_is_recognised() {
        assert!(is_websocket_upgrade(&headers(&[
            ("upgrade", "websocket"),
            ("connection", "Upgrade"),
        ])));
    }

    #[test]
    fn header_casing_and_token_lists_are_tolerated() {
        // Browsers and proxies send these in whatever case and order they like; RFC 9110 makes
        // both case-insensitive, and `Connection` is a comma-separated list.
        for (upgrade, connection) in [
            ("WebSocket", "upgrade"),
            ("WEBSOCKET", "UPGRADE"),
            ("websocket", "keep-alive, Upgrade"),
            ("websocket", "Upgrade, keep-alive"),
            ("websocket", "  upgrade  "),
        ] {
            assert!(
                is_websocket_upgrade(&headers(&[
                    ("upgrade", upgrade),
                    ("connection", connection)
                ])),
                "missed a valid handshake: upgrade={upgrade:?} connection={connection:?}"
            );
        }
    }

    #[test]
    fn half_a_handshake_is_not_a_handshake() {
        // Either header alone is an ordinary request. Treating it as an upgrade would hijack it.
        assert!(!is_websocket_upgrade(&headers(&[("upgrade", "websocket")])));
        assert!(!is_websocket_upgrade(&headers(&[(
            "connection",
            "Upgrade"
        )])));
        assert!(!is_websocket_upgrade(&HeaderMap::new()));
    }

    #[test]
    fn a_different_protocol_is_not_a_websocket() {
        // `Upgrade: h2c` is a real header that is not this.
        assert!(!is_websocket_upgrade(&headers(&[
            ("upgrade", "h2c"),
            ("connection", "Upgrade"),
        ])));
        assert!(!is_websocket_upgrade(&headers(&[
            ("upgrade", "websocket"),
            ("connection", "close"),
        ])));
    }

    #[test]
    fn a_substring_is_not_a_token() {
        // "upgraded" contains "upgrade"; splitting on commas and trimming is what stops a
        // substring match from counting as the token.
        assert!(!is_websocket_upgrade(&headers(&[
            ("upgrade", "websocket"),
            ("connection", "upgraded"),
        ])));
    }

    #[test]
    fn non_utf8_headers_do_not_panic() {
        let mut m = HeaderMap::new();
        m.append("upgrade", HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap());
        m.append("connection", HeaderValue::from_static("Upgrade"));
        assert!(!is_websocket_upgrade(&m));
    }
}
