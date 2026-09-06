//! Public ingress: `endpoint` nodes reachable from the internet.
//!
//! Mounted OUTSIDE the engine-secret layer, because the whole point is that a
//! webhook provider can reach it with nothing but a URL. The host proxies
//! `/p/<project>/<path>` here; nothing else on the engine is public.
//!
//! **The body reaches the agent through `Message::envelope`, not beside it.**
//! This module never formats an envelope and never calls the escaper. It calls
//! `db::messages::enqueue` with a `MessageSender::Node` whose type is
//! `Endpoint`, and the existing delivery loop does the rest — so `type` is
//! `endpoint` by construction rather than by a string this module could get
//! wrong, and the escaping fix that took the board down and was repaired today
//! is inherited rather than re-implemented (ADVERSARY 035).
//!
//! Order of work is deliberate and is the cost control on a public URL: match
//! the route, cap the body while reading it, rate-limit, authenticate, and only
//! then wake an agent. Every one of those rejects without touching a child.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json, Router,
};
use uuid::Uuid;
use wheel_core::{HttpMethod, MessageSender, NodeConfig, NodeType, WireType};

use super::AppState;
use crate::db::board;

/// Ceiling on a hit's body, enforced while reading rather than after.
///
/// A body becomes a delivered message, so this cannot exceed the message limit
/// — and buffering an unsigned 100 MB body to compute an HMAC over it would be
/// a memory DoS that costs the sender nothing (ADVERSARY 031, "size before
/// signature").
const MAX_INGRESS_BODY: usize = wheel_core::MAX_MESSAGE_BODY;

/// Hits allowed per client per window.
const RATE_LIMIT: u32 = 60;
const RATE_WINDOW: Duration = Duration::from_secs(60);

/// The header our OWN host sets to name the real caller.
///
/// Deliberately not `X-Forwarded-For`: any client can prepend one, so keying a
/// rate limit on it lets a caller mint a fresh identity per request. This name
/// is set by the host after it has seen the peer address, and is trusted for
/// no other purpose.
const TRUSTED_CLIENT_IP: &str = "x-wheel-client-ip";

#[derive(Default)]
pub struct RateLimiter {
    seen: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    /// True if this caller may proceed.
    pub fn allow(&self, key: &str) -> bool {
        let mut seen = match self.seen.lock() {
            Ok(s) => s,
            // A poisoned lock must not become an outage: fail open on the
            // limiter and closed on nothing.
            Err(p) => p.into_inner(),
        };
        let now = Instant::now();
        seen.retain(|_, (start, _)| now.duration_since(*start) < RATE_WINDOW);
        let entry = seen.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= RATE_WINDOW {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= RATE_LIMIT
    }
}

pub fn router() -> Router<AppState> {
    Router::new().fallback(handle)
}

/// A failed hit says nothing.
///
/// No body, no endpoint name, no hint whether the secret was wrong or the path
/// was: an ingress error is the one place where a helpful message is an oracle
/// for someone probing the board's shape (ADVERSARY 031, "no oracle").
fn unauthorised() -> Response {
    StatusCode::UNAUTHORIZED.into_response()
}

async fn handle(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let path = uri.path();

    // 1. ROUTE. Before anything expensive, and without reading the body.
    let wanted = match method.as_str() {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "DELETE" => HttpMethod::Delete,
        // A verb no endpoint can be configured for is indistinguishable from
        // a path that does not exist, and says less.
        _ => return (StatusCode::NOT_FOUND, Json(err("no_such_endpoint"))).into_response(),
    };
    let matched = {
        let conn = match state.db.lock() {
            Ok(c) => c,
            Err(p) => p.into_inner(),
        };
        match_endpoint(&conn, path, wanted)
    };
    let matched = match matched {
        Matched::Endpoint(e) => e,
        Matched::WrongMethod(allowed) => {
            // `Allow` names only THIS endpoint's method, never the board's
            // other endpoints on other paths.
            return (
                StatusCode::METHOD_NOT_ALLOWED,
                [("allow", allowed.as_str().to_string())],
                Json(err("method_not_allowed")),
            )
                .into_response();
        }
        Matched::None => {
            return (StatusCode::NOT_FOUND, Json(err("no_such_endpoint"))).into_response()
        }
    };

    // 2. RATE LIMIT, before the body is read and long before a child is woken.
    let client = headers
        .get(TRUSTED_CLIENT_IP)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<IpAddr>().ok().map(|ip| ip.to_string()))
        .unwrap_or_else(|| {
            // No trusted client header means the host did not name a caller.
            // One bucket rather than none: the limit still bounds what a public
            // URL can cost this project, which is what it is for.
            "unattributed".into()
        });
    if !state.ingress_rate.allow(&client) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }

    // 3. BODY, capped while reading. `to_bytes` stops at the limit rather than
    //    buffering first and measuring after.
    let Ok(raw) = axum::body::to_bytes(body, MAX_INGRESS_BODY).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };

    // 4. AUTHENTICATE. Optional by the operator's ruling: an endpoint that a
    //    webhook provider cannot be pointed at is not an endpoint.
    if !authenticate(&state, &matched, &headers, &uri, &raw) {
        return unauthorised();
    }

    // 5. Only now is an agent woken.
    deliver(&state, &matched, &method, path, &headers, &raw)
}

fn err(code: &str) -> serde_json::Value {
    serde_json::json!({ "code": code })
}

pub struct MatchedEndpoint {
    pub id: Uuid,
    pub name: wheel_core::NodeName,
    pub config: wheel_core::EndpointConfig,
}

pub enum Matched {
    Endpoint(Box<MatchedEndpoint>),
    WrongMethod(HttpMethod),
    None,
}

/// Find the endpoint node serving this path.
///
/// Path first, then method, so a path that exists with another verb answers
/// 405 with `Allow` rather than 404 — a webhook misconfigured as GET should be
/// told which verb it wants, and the path's existence is not a secret once the
/// caller already has the URL.
pub fn match_endpoint(conn: &rusqlite::Connection, path: &str, method: HttpMethod) -> Matched {
    let Ok(nodes) = board::list(conn) else {
        return Matched::None;
    };
    let mut wrong_method = None;
    for node in nodes {
        let NodeConfig::Endpoint(cfg) = &node.config else {
            continue;
        };
        if cfg.path != path {
            continue;
        }
        if cfg.method == method {
            return Matched::Endpoint(Box::new(MatchedEndpoint {
                id: node.id,
                name: node.name.clone(),
                config: cfg.clone(),
            }));
        }
        wrong_method = Some(cfg.method);
    }
    match wrong_method {
        Some(m) => Matched::WrongMethod(m),
        None => Matched::None,
    }
}

/// Does this hit present the endpoint's secret?
///
/// `None` (the default) is public and returns true — the operator's ruling,
/// and the reason it is stated here rather than left implicit: an endpoint you
/// cannot point a webhook provider at is not an endpoint.
fn authenticate(
    state: &AppState,
    matched: &MatchedEndpoint,
    headers: &HeaderMap,
    uri: &Uri,
    _raw: &[u8],
) -> bool {
    use wheel_core::EndpointAuth;
    let vault_ref = match &matched.config.auth {
        EndpointAuth::None => return true,
        EndpointAuth::Bearer { vault_ref } => vault_ref,
    };

    // The secret is read through the endpoint's OWN wires, so an endpoint
    // without a `read` wire to the vault cannot authenticate at all — the
    // capability is the wire, here as everywhere else.
    let Some(expected) = resolve_secret(state, matched.id, vault_ref) else {
        return false;
    };

    // `authorization: Bearer <secret>`, or the same value in the header a
    // provider can actually set. Telegram's setWebhook cannot send arbitrary
    // headers but CAN send `x-telegram-bot-api-secret-token`, so a raw header
    // match is a first-class case, not a fallback.
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::to_string)
        .or_else(|| {
            headers
                .get("x-telegram-bot-api-secret-token")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        })
        .or_else(|| {
            headers
                .get("x-wheel-secret")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        })
        .or_else(|| {
            // A sender that can be given only a URL gets a longer URL.
            uri.query().and_then(|q| {
                q.split('&')
                    .filter_map(|kv| kv.split_once('='))
                    .find(|(k, _)| *k == "token")
                    .map(|(_, v)| v.to_string())
            })
        });

    match presented {
        Some(p) => super::constant_time_eq(p.as_bytes(), expected.as_bytes()),
        None => false,
    }
}

/// Read `<vault>/<key>`, but only across a real `endpoint → vault (read)` wire.
fn resolve_secret(state: &AppState, endpoint: Uuid, vault_ref: &str) -> Option<String> {
    let (vault_name, key) = vault_ref.split_once('/')?;
    let conn = match state.db.lock() {
        Ok(c) => c,
        Err(p) => p.into_inner(),
    };
    let wires = board::wires_from(&conn, endpoint).ok()?;
    let nodes = board::list(&conn).ok()?;
    let vault = nodes.iter().find(|n| {
        n.name.as_str() == vault_name
            && n.node_type() == NodeType::Vault
            && wires
                .iter()
                .any(|w| w.to == n.id && w.wire_type == WireType::Read)
    })?;
    let vk = state.supervisor.require_vault_key().ok()?;
    crate::vault::get(&conn, vk, vault.id, key).ok().flatten()
}

/// Fan the hit out over the endpoint's own wires.
///
/// The body is delivered RAW. The engine does not parse Telegram, GitHub or
/// anyone else's payload shape — the agent reads it. That is what keeps an
/// endpoint node provider-agnostic, which is the whole point of having one
/// node type rather than a node type per webhook vendor.
fn deliver(
    state: &AppState,
    matched: &MatchedEndpoint,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    raw: &[u8],
) -> Response {
    let body = envelope_payload(method, path, headers, raw);

    let conn = match state.db.lock() {
        Ok(c) => c,
        Err(p) => p.into_inner(),
    };
    let Ok(wires) = board::wires_from(&conn, matched.id) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let sender = MessageSender::Node {
        id: matched.id,
        name: matched.name.clone(),
        node_type: NodeType::Endpoint,
    };

    let mut delivered = 0usize;
    for wire in wires.iter().filter(|w| w.wire_type == WireType::Send) {
        // `enqueue` is the ONLY delivery path. It is what makes the body reach
        // the child through `Message::envelope` — so `type="endpoint"` and the
        // escaping are properties of the message type, not of this module.
        if crate::db::messages::enqueue(&conn, sender.clone(), wire.to, body.clone(), None).is_ok()
        {
            delivered += 1;
            let supervisor = state.supervisor.clone();
            let target = wire.to;
            tokio::spawn(async move {
                let _ = supervisor.start(target).await;
            });
        }
    }
    drop(conn);

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "accepted": true, "delivered": delivered })),
    )
        .into_response()
}

/// What the agent actually sees: the request, as JSON, with the credential
/// removed.
///
/// The presented secret must not reach the delivered message, the transcript or
/// the log — an agent that can echo its own prompt would otherwise publish the
/// endpoint's secret, and the transcript is stored.
fn envelope_payload(method: &Method, path: &str, headers: &HeaderMap, raw: &[u8]) -> String {
    const REDACTED: [&str; 5] = [
        "authorization",
        "x-telegram-bot-api-secret-token",
        "x-wheel-secret",
        "cookie",
        "proxy-authorization",
    ];
    let mut safe = serde_json::Map::new();
    for (name, value) in headers {
        let key = name.as_str().to_ascii_lowercase();
        if REDACTED.contains(&key.as_str()) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            safe.insert(key, serde_json::Value::String(v.to_string()));
        }
    }
    // Text if it is text, so an agent reading JSON sees JSON rather than an
    // escaped string of it; bytes otherwise, rather than lossy nonsense.
    let body = match std::str::from_utf8(raw) {
        Ok(text) => serde_json::Value::String(text.to_string()),
        Err(_) => serde_json::json!({ "bytes": raw.len(), "utf8": false }),
    };
    serde_json::json!({
        "method": method.as_str(),
        "path": path,
        "headers": safe,
        "body": body,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADVERSARY 035's open link, asserted structurally rather than argued.
    ///
    /// The whole poison chain collapses to one sink — `Message::envelope`,
    /// which calls the escaper that took the board down today. Ingress built
    /// BESIDE that sink would re-open the hole silently, and no test of
    /// ingress behaviour would notice, because the bug is an absence.
    ///
    /// So this reads the module: ingress must not format an envelope, must not
    /// call the escaper, and must reach an agent only through
    /// `messages::enqueue`. A future edit that hand-rolls delivery here fails
    /// this even if it produces byte-identical output today.
    #[test]
    fn ingress_delivers_only_through_the_one_envelope_sink() {
        let src = include_str!("ingress.rs");
        // Ignore this test module itself, which necessarily names them.
        let code = src.split("#[cfg(test)]").next().unwrap_or_default();

        assert!(
            !code.contains("AgentPrompt"),
            "ingress formats an envelope of its own; it must let Message::envelope do it"
        );
        assert!(
            !code.contains("escape_envelope_body"),
            "ingress calls the escaper directly; delivery must go through Message"
        );
        assert!(
            code.contains("messages::enqueue"),
            "ingress must deliver through messages::enqueue, the only path that \
             reaches Message::envelope"
        );
    }

    /// The hit is attributed to the endpoint NODE, so the envelope's `type` is
    /// `endpoint` because of what the sender IS — not because this module
    /// wrote the word. `type=user` is the operator's own turns and an external
    /// caller must never be able to wear it.
    #[test]
    fn a_hit_is_attributed_to_the_endpoint_and_never_to_the_user() {
        let name = wheel_core::NodeName::new("tg").unwrap();
        let sender = MessageSender::Node {
            id: Uuid::new_v4(),
            name,
            node_type: NodeType::Endpoint,
        };
        assert_eq!(sender.sender_type(), "endpoint");
        assert_ne!(sender.sender_type(), "user");

        let code = include_str!("ingress.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap_or_default();
        assert!(
            !code.contains("MessageSender::User"),
            "ingress must never attribute a hit to the operator"
        );
    }

    /// The presented credential must not reach the delivered message: the body
    /// becomes a transcript, and an agent that echoes its prompt would publish
    /// the endpoint's secret.
    #[test]
    fn the_presented_credential_never_reaches_the_delivered_body() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            "Bearer super-secret-value".parse().unwrap(),
        );
        headers.insert(
            "x-telegram-bot-api-secret-token",
            "telegram-secret".parse().unwrap(),
        );
        headers.insert("content-type", "application/json".parse().unwrap());

        let payload = envelope_payload(&Method::POST, "/tg", &headers, br#"{"ok":true}"#);

        assert!(!payload.contains("super-secret-value"), "{payload}");
        assert!(!payload.contains("telegram-secret"), "{payload}");
        // ...while the useful part of the request survives.
        assert!(payload.contains("application/json"), "{payload}");
        assert!(payload.contains("\\\"ok\\\":true"), "{payload}");
    }

    /// A body that is not UTF-8 is described rather than mangled: a lossy
    /// conversion would hand the agent invented characters and call them the
    /// request.
    #[test]
    fn a_non_utf8_body_is_described_rather_than_mangled() {
        let payload = envelope_payload(&Method::POST, "/x", &HeaderMap::new(), &[0xff, 0xfe, 0x00]);
        assert!(payload.contains("\"utf8\":false"), "{payload}");
    }

    /// The rate limit is the cost control on a public URL, so it must actually
    /// stop something.
    #[test]
    fn the_rate_limiter_stops_a_caller_past_the_window_budget() {
        let limiter = RateLimiter::default();
        for i in 0..RATE_LIMIT {
            assert!(limiter.allow("1.2.3.4"), "rejected legitimate hit {i}");
        }
        assert!(!limiter.allow("1.2.3.4"), "the budget was not enforced");
        // A different caller has its own budget.
        assert!(
            limiter.allow("5.6.7.8"),
            "one caller exhausted another's budget"
        );
    }

    /// The cap is on the message limit, because the body BECOMES a message —
    /// and it is applied while reading, so an oversized body is never buffered.
    ///
    /// Stated as an equality against the source of truth rather than an
    /// `assert!` clippy can fold away: the point is that this constant tracks
    /// the message limit, not that today's numbers happen to compare.
    #[test]
    fn the_body_cap_is_the_message_limit() {
        assert_eq!(MAX_INGRESS_BODY, wheel_core::MAX_MESSAGE_BODY);
    }
}
