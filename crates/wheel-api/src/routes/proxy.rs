// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Authenticated proxy to a project's engine, via the host.
//!
//! Trust boundary notes:
//!   * The handler takes `ProjectScope`, so membership is proven before a single byte is forwarded,
//!     and `auth::policy` then decides whether the caller's tier reaches this engine path. The
//!     table is default-DENY, so an engine route nobody wrote a rule for is unreachable here.
//!   * **The `x-wheel-` namespace is stripped from the client's request and then set by us.** Until
//!     this change only the public ingress route did that, so an authenticated tenant could send
//!     any `x-wheel-*` header and it reached the engine untouched (`wheel-host` relays anything it
//!     is not explicitly told to drop). Nothing consumed those headers on this path, so it was
//!     latent rather than live — but the actor markers below are exactly the thing that would have
//!     made it identity forgery. See
//!     `redteam/findings/052-authenticated-proxy-does-not-strip-x-wheel-namespace.md`.
//!   * `WHEEL_HOST_SECRET` is attached here and never travels back to the client. The client's own
//!     credentials are stripped by `sanitize_for_upstream` — the host authenticates *us*, not the
//!     user, and relaying a user token downstream is how replay bugs start.
//!   * The upstream URL is the project's host base, built from a `Uuid` we loaded from our own
//!     database, with the caller's suffix appended by `wheel_core::proxy_path`. Concatenating the
//!     suffix instead let an encoded `..` be decoded again by the URL parser and climb into another
//!     project's route on the host, which answers with that project's engine secret.

use crate::auth::{policy, AuthUser, ProjectScope, Tier};
use crate::error::{ApiError, ApiResult};
use crate::http::{actor, hop};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::ws::{Message as AxumMsg, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, Request, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as TungMsg;
use wheel_core::proxy_path::{self, ProxyPathError, Url};

/// Ceiling on the upstream WebSocket handshake. Generous for a healthy engine on the same private
/// network, and short enough that a stalled peer cannot hold the connection open indefinitely.
const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the bridge pings the client, and how long it then waits for a pong.
///
/// ADVERSARY 011 is explicit that a plain idle-read timeout cannot tell a dead peer from a
/// legitimately idle one on a long-lived push channel — the events socket is silent by design when
/// nothing is happening. A ping the peer must answer can.
const WS_KEEPALIVE: Duration = Duration::from_secs(30);

/// How often a live bridge re-checks that its holder is still a member at the same tier.
///
/// This is the control that makes revocation *certain* rather than fast: NOTIFY closes a socket in
/// milliseconds when it arrives, and this closes it even when it does not — a dropped listener, a
/// row changed outside the API, a backend with no NOTIFY.
const WS_MEMBERSHIP_RECHECK: Duration = Duration::from_secs(30);

pub async fn engine_proxy(
    State(state): State<AppState>,
    scope: ProjectScope,
    Path((_id, rest)): Path<(uuid::Uuid, String)>,
    req: Request,
) -> ApiResult<Response> {
    // Decode once, and authorise against the very segments the upstream URL will be built from.
    // Matching the raw suffix instead would mean the policy read one path while the engine served
    // another, which is the confusion this proxy already refuses for `x-project-id`.
    let segments = proxy_path::proxy_segments(&rest).map_err(|_| path_refused())?;
    require_engine_tier(&scope, req.method(), &segments)?;

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
        bridge_websocket(state, upgrade, upstream, scope).await
    } else {
        forward_http(state, req, upstream, &scope.user, scope.tier).await
    }
}

/// Refuse the request unless the caller's tier reaches what this engine path requires.
///
/// A path with no rule is refused, not allowed: `auth::policy` is default-DENY, so adding an engine
/// route without a rule makes it unreachable through the API rather than reachable by everyone.
fn require_engine_tier(
    scope: &ProjectScope,
    method: &axum::http::Method,
    segments: &[&str],
) -> ApiResult<()> {
    let needed = policy::engine_tier(method, segments).ok_or(ApiError::Forbidden(
        "this engine path is not reachable through the API",
    ))?;
    scope.require(needed)
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
/// member for one specific project, survives 30 seconds, and is consumed on first use.
///
/// This is the one route whose scope is built by hand, because its two doors reach an identity by
/// different routes and only one of them produces an `AuthUser` from a header. Both end at the same
/// `ProjectScope`, so the tier check below is written once.
pub async fn engine_events(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    req: Request,
) -> ApiResult<Response> {
    let (mut parts, body) = req.into_parts();
    let raw_query = parts.uri.query().unwrap_or("").to_string();

    let scope = match query_param(&raw_query, "ticket") {
        Some(ticket) => {
            // Redemption proves the caller was a member when the ticket was minted, and binds it to
            // this project specifically. The user id it returns used to be discarded; it is the
            // identity of whoever opened this socket, and it is what the actor header below and
            // every attributed message on this connection are named after.
            let user_id = crate::routes::ws_ticket::redeem(&state, &ticket, &id).await?;
            // Membership is resolved HERE, at redemption, never at mint. Same lesson `api_token`
            // records for minting chains: a ticket minted a moment before a revocation landed and
            // redeemed a moment after must not open a socket, and only a check at use time sees it.
            ProjectScope::from_redeemed_ticket(&state, user_id, &id).await?
        }
        None => {
            // No ticket: the ordinary header-authenticated path, which also proves membership.
            // Either way we do not reach the engine without one of the two.
            ProjectScope::from_request_parts(&mut parts, &state).await?
        }
    };
    // The REAL method, not `GET`. This route forwards whatever verb it was called with, so
    // checking a hardcoded `GET` and then forwarding a `DELETE` authorises one request and performs
    // another — the confusion this proxy refuses everywhere else. Latent only because the engine
    // registers `get("/events")` and answers 405 to the rest; a table consulted about a method
    // nobody used is not a table.
    require_engine_tier(&scope, &parts.method, &["v1", "events"])?;

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
        bridge_websocket(state, upgrade, upstream, scope).await
    } else {
        forward_http(state, req, upstream, &scope.user, scope.tier).await
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

async fn forward_http(
    state: AppState,
    req: Request,
    upstream: Url,
    user: &AuthUser,
    tier: Tier,
) -> ApiResult<Response> {
    let method = req.method().clone();
    // Strip the whole `x-wheel-` namespace, then set ours. A caller who forged
    // `x-wheel-actor-tier: admin` has it removed and *replaced* with their real tier — not merely
    // ignored, which would leave their value in the map beside ours.
    let headers = actor::sanitized_with_actor(req.headers(), user, tier);

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
///
/// # What bounds an established bridge (ADVERSARY 011)
///
/// The handshake timeout below bounds only the handshake. Once a bridge exists it is long-lived by
/// design, so four separate things end it: the per-project cap refuses it before it starts, the
/// keepalive closes a peer that stops answering, the membership watch closes it when access
/// changes, and the lifetime cap closes it regardless.
async fn bridge_websocket(
    state: AppState,
    upgrade: WebSocketUpgrade,
    upstream: Url,
    scope: ProjectScope,
) -> ApiResult<Response> {
    // Taken before the upstream connect, so a project at its ceiling cannot make us open sockets to
    // the host in order to find that out.
    let slot = state
        .bridges
        .acquire(scope.project.id, state.cfg.ws_max_bridges_per_project)
        .ok_or_else(|| {
            tracing::warn!(project_id = %scope.project.id, "websocket bridge cap reached");
            // 503, not 429 and not 502: the limit is a concurrency ceiling on this replica rather
            // than a rate, and nothing upstream is wrong. Retrying when one closes is exactly the
            // right thing for a client to do.
            ApiError::ServiceUnavailable("websocket bridge cap reached")
        })?;

    let ws_url = proxy_path::websocket_url(upstream).ok_or_else(|| {
        ApiError::Internal(anyhow::anyhow!("the host base URL has no websocket scheme"))
    })?;

    let mut request = tokio_tungstenite::tungstenite::http::Request::builder()
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
        );

    // This path builds a FRESH request and forwards no client header at all, so
    // `sanitize_for_upstream` never runs here and there is nothing to strip — but equally, nothing
    // of ours is present unless it is put here explicitly. Assuming the HTTP path's behaviour
    // covers this one is the mistake available at this line.
    {
        let mut actor_headers = axum::http::HeaderMap::new();
        actor::set_actor(&mut actor_headers, &scope.user, scope.tier);
        for (name, value) in actor_headers.iter() {
            request = request.header(name, value);
        }
    }

    let request = request
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

    let watch = BridgeWatch {
        state: state.clone(),
        project_id: scope.project.id,
        user_id: scope.user.id().to_string(),
        tier: scope.tier,
    };
    Ok(upgrade.on_upgrade(move |client| {
        // The slot rides into the pump and is released when this future ends, however it ends.
        async move {
            pump(client, upstream, watch).await;
            drop(slot);
        }
    }))
}

/// What a live bridge keeps re-checking about the person holding it.
struct BridgeWatch {
    state: AppState,
    project_id: uuid::Uuid,
    user_id: String,
    tier: Tier,
}

impl BridgeWatch {
    /// Is this bridge's holder still entitled to it?
    ///
    /// A *downgrade* ends it too, not only a revocation: a demoted admin otherwise keeps an
    /// admin-tier socket until it happens to close on its own. Re-opening at the new tier is one
    /// round trip and is the client's to do.
    async fn still_entitled(&self) -> bool {
        match crate::auth::extractor::load_member(&self.state, &self.project_id, &self.user_id)
            .await
        {
            Ok((_, tier)) => tier >= self.tier,
            // A database error is not evidence of revocation, and closing every socket in the
            // deployment because Postgres blinked would be an outage we caused. The lifetime cap
            // still bounds how long that can persist.
            Err(ApiError::NotFound) => false,
            Err(e) => {
                tracing::warn!(error = ?e, "membership re-check failed; leaving the bridge open");
                true
            }
        }
    }
}

async fn pump(
    client: WebSocket,
    upstream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    watch: BridgeWatch,
) {
    let (mut client_tx, mut client_rx) = client.split();
    let (mut up_tx, mut up_rx) = upstream.split();

    let deadline = tokio::time::sleep(Duration::from_secs(watch.state.cfg.ws_max_lifetime_secs));
    tokio::pin!(deadline);
    // `interval_at`, not `interval`: a plain interval fires immediately, which would ping before
    // the client has had a chance to exist and re-check membership that was resolved microseconds
    // ago. Both ticks want to start one period from now.
    let start = tokio::time::Instant::now();
    let mut keepalive = tokio::time::interval_at(start + WS_KEEPALIVE, WS_KEEPALIVE);
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut recheck =
        tokio::time::interval_at(start + WS_MEMBERSHIP_RECHECK, WS_MEMBERSHIP_RECHECK);
    recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut access = watch.state.membership.subscribe();

    // Set when a ping goes out, cleared by the answering pong. A second ping finding it still set
    // means the peer did not answer within the interval, which is the distinction a plain read
    // timeout cannot draw on a channel that is silent by design.
    let mut awaiting_pong = false;
    let mut why: &'static str = "peer closed";

    loop {
        // Deliberately NOT `biased`. Biasing the ending conditions ahead of the traffic arms reads
        // as the safer order, but it means a branch that is always ready — `access.recv()` under a
        // burst of membership changes — starves the relay entirely. Random selection cannot starve
        // anything, and what it costs is at most one more frame of a read-only push stream reaching
        // someone whose access ended microseconds ago. That is a worse trade in a comment than it is
        // in practice.
        tokio::select! {
            _ = &mut deadline => {
                why = "lifetime cap reached";
                break;
            }

            change = access.recv() => {
                match change {
                    Ok(c) if c.project_id == watch.project_id && c.user_id == watch.user_id => {
                        if !watch.still_entitled().await {
                            why = "access revoked";
                            break;
                        }
                    }
                    // Lagged means we missed notifications. Rather than guess, re-check: the
                    // answer is one query and it is the same answer the periodic tick would get.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if !watch.still_entitled().await {
                            why = "access revoked";
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {}
                    Ok(_) => {}
                }
            }

            _ = recheck.tick() => {
                if !watch.still_entitled().await {
                    why = "access revoked";
                    break;
                }
            }

            _ = keepalive.tick() => {
                if awaiting_pong {
                    why = "no pong within the keepalive deadline";
                    break;
                }
                if client_tx.send(AxumMsg::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
                awaiting_pong = true;
            }

            incoming = client_rx.next() => {
                let Some(Ok(msg)) = incoming else { break };
                let out = match msg {
                    AxumMsg::Text(t) => TungMsg::Text(t.as_str().into()),
                    AxumMsg::Binary(b) => TungMsg::Binary(b),
                    AxumMsg::Ping(p) => TungMsg::Ping(p),
                    AxumMsg::Pong(p) => {
                        // Answers our keepalive; also relayed, so an upstream ping still works.
                        awaiting_pong = false;
                        TungMsg::Pong(p)
                    }
                    AxumMsg::Close(_) => break,
                };
                if up_tx.send(out).await.is_err() {
                    break;
                }
            }

            outgoing = up_rx.next() => {
                let Some(Ok(msg)) = outgoing else { break };
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
        }
    }

    tracing::debug!(
        project_id = %watch.project_id,
        reason = why,
        "websocket bridge closed"
    );
    let _ = up_tx.close().await;
    let _ = client_tx.close().await;
}
