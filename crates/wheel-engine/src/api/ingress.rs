// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

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
use wheel_core::{ErrorBody, HttpMethod, MessageSender, NodeConfig, NodeType, WireType};

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
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(err("no_such_endpoint", "no endpoint answers this path")),
            )
                .into_response()
        }
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
                Json(err(
                    "method_not_allowed",
                    "this path does not accept this method",
                )),
            )
                .into_response();
        }
        Matched::None => {
            return (
                StatusCode::NOT_FOUND,
                Json(err("no_such_endpoint", "no endpoint answers this path")),
            )
                .into_response()
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

/// The same `{"error":{"code","message"}}` envelope every other engine route
/// uses (`api/mod.rs`'s `ApiError`) — this module builds its own responses
/// rather than going through `ApiError` (ingress needs a bare `Response` for
/// the deliver path, and `Allow`/rate-limit headers besides), but a bare
/// `{"code":...}` here would still be a second, disagreeing error contract on
/// the same engine.
fn err(code: &str, message: &str) -> ErrorBody {
    ErrorBody::new(code, message)
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
    let expected = match resolve_secret(state, matched, vault_ref) {
        Ok(secret) => secret,
        Err(reason) => {
            // An operator-error case (misconfiguration, or the vault/wire/key
            // state this build expects is missing), never a wrong-credential
            // case — the caller's presented value is not even looked at yet.
            // Logged, unlike a bad credential: hiding a WRONG presented
            // secret protects against enumeration; hiding a BROKEN endpoint
            // just leaves the operator staring at silent 401s (this bug).
            tracing::warn!(
                endpoint = %matched.name,
                endpoint_id = %matched.id,
                reason = reason.as_str(),
                "auth:bearer endpoint cannot resolve its secret — every request will 401 \
                 until this is fixed"
            );
            return false;
        }
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
            // Percent-decoded: this is a query VALUE, not a comparison of raw
            // wire bytes, and a secret containing `%`, `&`, `+` or any other
            // URL-reserved byte arrives here still encoded (ADVERSARY,
            // review of #103). `+` is left as a literal plus -- this is a URI
            // query component per RFC 3986, not
            // `application/x-www-form-urlencoded`, which is the one place
            // `+` means space.
            uri.query().and_then(|q| {
                q.split('&')
                    .filter_map(|kv| kv.split_once('='))
                    .find(|(k, _)| *k == "token")
                    // STRICT utf8, not `from_utf8_lossy`: a decoded value
                    // that is not valid utf8 must never reach the comparison
                    // as a lossy string (ADVERSARY, review of #103) -- U+FFFD
                    // substitution is a many-to-one mapping, and a security
                    // comparison must not have one, even where nothing today
                    // exploits it (every real secret is a `String`, so
                    // `expected` cannot itself contain the substitution
                    // character for a collision to land on). Failing to
                    // produce a candidate here is just "no credential
                    // presented" -- the existing, already-safe 401 path.
                    .and_then(|(_, v)| String::from_utf8(percent_decode_bytes(v)).ok())
            })
        });

    match presented {
        Some(p) => super::constant_time_eq(p.as_bytes(), expected.as_bytes()),
        None => false,
    }
}

/// Percent-decode a single query-string VALUE (not a whole URL) to its raw
/// bytes. `%XX` becomes that byte; anything else, including a `%` not
/// followed by two hex digits, passes through literally rather than being
/// dropped -- a malformed escape must not silently shorten the value into
/// something that then coincidentally fails to match for a second, unrelated
/// reason.
///
/// Returns bytes, not a `String`: the one caller compares against a secret on
/// a security-sensitive path, and `String::from_utf8_lossy` would substitute
/// U+FFFD for any invalid sequence -- a many-to-one mapping with no place on
/// a comparison, however impractical to exploit today (ADVERSARY, review of
/// #103).
fn percent_decode_bytes(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Every distinguishable reason `resolve_secret` can fail to produce a
/// secret. Previously collapsed into one bare `None`, which meant an
/// operator's own broken wiring (missing wire, wrong vault_ref, no vault key
/// on this deployment, a value that will not decrypt) looked EXACTLY like the
/// "working as designed" case of an endpoint nobody has wired a vault to yet
/// — and both looked, from the outside, like a caller's wrong credential. The
/// three names below are `authenticate`'s own log line, not exposed to the
/// caller: a wrong credential still gets a bare 401, unchanged.
#[derive(Debug, PartialEq, Eq)]
enum SecretError {
    /// `vault_ref` is not `<name>/<key>` — a config-time bug, since
    /// `EndpointAuth::Bearer`'s `vault_ref` is operator-entered free text.
    MalformedRef,
    /// No node named `<name>`, of type vault, that this endpoint holds a
    /// `read` wire to. Covers "wrong name", "right name wrong type", and
    /// "right vault but the wire was never drawn" as one case: from the
    /// endpoint's point of view they are the same fact — it cannot reach it.
    NoReadableVault,
    /// The vault exists and is wired, but has nothing stored at `<key>`.
    NoSuchKey,
    /// This engine has no usable vault key at all (`WHEEL_VAULT_KEY` missing
    /// or unparseable) — a deployment-level fact, true for every vault on the
    /// board, not particular to this endpoint.
    NoVaultKey(&'static str),
    /// A value is stored at `<key>`, but would not decrypt under this
    /// engine's vault key. The one case that is otherwise SILENT by
    /// construction (`vault::get` returns `Err`, previously swallowed by
    /// `.ok()`): a vault key rotated without re-encrypting stored values
    /// leaves every secret behind it permanently unreadable, with nothing
    /// short of this log line to say so.
    DecryptFailed,
    /// The board's own storage could not be read at all (a poisoned lock
    /// aside, effectively unreachable) — named separately so it is never
    /// confused with a legitimate "not found".
    StorageUnavailable,
}

impl SecretError {
    fn as_str(&self) -> &'static str {
        match self {
            SecretError::MalformedRef => "malformed_vault_ref",
            SecretError::NoReadableVault => "no_readable_vault",
            SecretError::NoSuchKey => "no_such_key",
            SecretError::NoVaultKey(_) => "no_vault_key",
            SecretError::DecryptFailed => "decrypt_failed",
            SecretError::StorageUnavailable => "storage_unavailable",
        }
    }
}

/// Read `<vault>/<key>`, but only across a real `endpoint → vault (read)` wire.
fn resolve_secret(
    state: &AppState,
    matched: &MatchedEndpoint,
    vault_ref: &str,
) -> Result<String, SecretError> {
    let (vault_name, key) = vault_ref.split_once('/').ok_or(SecretError::MalformedRef)?;
    let conn = match state.db.lock() {
        Ok(c) => c,
        Err(p) => p.into_inner(),
    };
    let wires =
        board::wires_from(&conn, matched.id).map_err(|_| SecretError::StorageUnavailable)?;
    let nodes = board::list(&conn).map_err(|_| SecretError::StorageUnavailable)?;
    let vault = nodes
        .iter()
        .find(|n| {
            n.name.as_str() == vault_name
                && n.node_type() == NodeType::Vault
                && wires
                    .iter()
                    .any(|w| w.to == n.id && w.wire_type == WireType::Read)
        })
        .ok_or(SecretError::NoReadableVault)?;
    let vk = state
        .supervisor
        .require_vault_key()
        .map_err(SecretError::NoVaultKey)?;
    match crate::vault::get(&conn, vk, vault.id, key) {
        Ok(Some(secret)) => Ok(secret),
        Ok(None) => Err(SecretError::NoSuchKey),
        Err(_) => Err(SecretError::DecryptFailed),
    }
}

/// Fan the hit out over the endpoint's own wires.
///
/// The body is delivered RAW. The engine does not parse Telegram, GitHub or
/// anyone else's payload shape — the agent reads it. That is what keeps an
/// endpoint node provider-agnostic, which is the whole point of having one
/// node type rather than a node type per webhook vendor.
pub(crate) fn deliver(
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

    let mut queued = 0usize;
    for wire in wires.iter().filter(|w| w.wire_type == WireType::Send) {
        // `enqueue` is the ONLY delivery path. It is what makes the body reach
        // the child through `Message::envelope` — so `type="endpoint"` and the
        // escaping are properties of the message type, not of this module.
        // No actor, by design. An ingress hit is anonymous and arrives as `type=endpoint`; a tier
        // must never become a way into this route, and this route must never become a way to
        // attribute a message to a person who did not send it.
        if crate::db::messages::enqueue(&conn, sender.clone(), wire.to, body.clone(), None, None)
            .is_ok()
        {
            queued += 1;
            let supervisor = state.supervisor.clone();
            let target = wire.to;
            tokio::spawn(async move {
                // `deliver`, never `start`. `start` is idempotent by design
                // (§3c#13), so against an agent already running, idle, or
                // wedged in `starting` it returns the existing session having
                // pumped nothing, and the row just written waits for an event
                // that never comes. That stranded the operator's board: three
                // messages queued behind an agent frozen in `starting` while
                // ingress answered 202. Every other enqueue path in the engine
                // calls `deliver`; this was the one exception.
                let _ = supervisor.deliver(target).await;
            });
        }
    }
    drop(conn);

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "accepted": true, "queued": queued })),
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

    /// The P0 that stranded the operator's board: three messages queued, none
    /// delivered, the `pm` agent frozen in `starting`, and ingress answering
    /// 202 the whole time.
    ///
    /// `start` is IDEMPOTENT by design (§3c#13: a message must never spawn a
    /// second process for one agent). Against an agent that is already
    /// running, idle, or wedged in `starting`, it returns the existing session
    /// having pumped nothing -- so the row ingress just wrote waits for an
    /// event that never comes. `deliver` resumes a parked agent AND drains the
    /// queue, which is why every other enqueue path calls it: agent_routes in
    /// four places, cli_routes in one, and ingress was the exception.
    ///
    /// Asserted on the source because the defect is a WRONG CALL, not a wrong
    /// value: with `start` the handler still answers 202 and still writes the
    /// row, so nothing observable about the response distinguishes the broken
    /// engine from the fixed one. The behavioural form of this belongs in QA's
    /// ING-* suite, where a live engine and a real child exist; there is no
    /// harness in this module that can watch a child's stdin.
    #[test]
    fn an_ingress_hit_pumps_the_queue_rather_than_only_starting_the_agent() {
        let src = include_str!("ingress.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or_default();

        assert!(
            code.contains("supervisor.deliver("),
            "ingress must reach the agent through supervisor.deliver: it is the only \
             path that drains the queue for an agent that is already running"
        );
        assert!(
            !code.contains("supervisor.start("),
            "ingress must not reach an agent with supervisor.start — it is a no-op \
             against a running or wedged agent and leaves the message queued for ever"
        );
    }

    /// `delivered` is a defined state in the protocol (§3c#4: the bytes reached
    /// the child's stdin), and this response is written before any child is
    /// touched. Filed independently by API and Web: the word made a webhook
    /// provider mark as succeeded a hook that no agent had seen, so it never
    /// retried.
    #[test]
    fn the_202_body_names_the_state_it_actually_wrote() {
        let src = include_str!("ingress.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or_default();
        assert!(
            code.contains("\"queued\": queued"),
            "the ingress 202 must report `queued`, which is the state it wrote"
        );
        assert!(
            !code.contains("\"delivered\":"),
            "`delivered` in the ingress body claims a state the engine has not reached"
        );
    }

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

    /// Web caught this live: `err()` built a bare `{"code":...}` body, which
    /// disagrees with every other engine route's `{"error":{"code",
    /// "message"}}` envelope (`api/mod.rs`'s `ApiError`/`ErrorBody`) --
    /// including `crates/wheel-api/tests/ingress_honesty.rs`'s own mock of the
    /// wrapped shape. One error contract for the whole engine, ingress
    /// included.
    #[test]
    fn ingress_errors_use_the_same_envelope_as_every_other_engine_route() {
        let body = err("no_such_endpoint", "no endpoint answers this path");
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["error"]["code"], "no_such_endpoint");
        assert_eq!(v["error"]["message"], "no endpoint answers this path");
        // Not a second, bare `code` sitting beside `error` -- exactly one
        // top-level key, matching ApiError's own IntoResponse.
        assert_eq!(
            v.as_object().unwrap().len(),
            1,
            "unexpected top-level shape: {v}"
        );
    }

    /// PM/Morgan's repro (project `ingress-auth-repro`): a correctly wired,
    /// correctly configured, correctly presented Bearer secret must
    /// authenticate. It does not -- `authenticate()` returns false for every
    /// valid presentation form, which fails closed (safe direction) but means
    /// every `auth:bearer` endpoint on the board is permanently unusable.
    #[test]
    fn a_bearer_secret_actually_authenticates_a_real_wired_endpoint() {
        use wheel_core::{
            AgentConfig, EndpointAuth, EndpointConfig, HttpMethod, Node, Position, ResponseMode,
            VaultConfig,
        };

        let state = crate::api::test_state();
        let conn = state.db.lock().unwrap();

        let vault = Node::new(
            Uuid::new_v4(),
            "v".parse().unwrap(),
            Position::default(),
            NodeConfig::Vault(VaultConfig {
                keys: vec!["S".into()],
            }),
        );
        let agent = Node::new(
            Uuid::new_v4(),
            "a".parse().unwrap(),
            Position::default(),
            NodeConfig::Agent(AgentConfig::default()),
        );
        let endpoint = Node::new(
            Uuid::new_v4(),
            "e".parse().unwrap(),
            Position::default(),
            NodeConfig::Endpoint(EndpointConfig {
                method: HttpMethod::Post,
                path: "/hook".into(),
                response_mode: ResponseMode::Ack,
                auth: EndpointAuth::Bearer {
                    vault_ref: "v/S".into(),
                },
            }),
        );
        board::create(&conn, &vault).unwrap();
        board::create(&conn, &agent).unwrap();
        board::create(&conn, &endpoint).unwrap();
        board::add_wire(&conn, endpoint.id, vault.id, WireType::Read, None).unwrap();
        board::add_wire(&conn, endpoint.id, agent.id, WireType::Send, None).unwrap();

        let vk = state.supervisor.require_vault_key().unwrap();
        crate::vault::put(&conn, vk, vault.id, "S", "the-secret").unwrap();
        drop(conn);

        let matched = MatchedEndpoint {
            id: endpoint.id,
            name: endpoint.name.clone(),
            config: match &endpoint.config {
                NodeConfig::Endpoint(c) => c.clone(),
                _ => unreachable!(),
            },
        };
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer the-secret".parse().unwrap());
        let uri: Uri = "/hook".parse().unwrap();

        assert!(
            authenticate(&state, &matched, &headers, &uri, b""),
            "a correctly wired endpoint with the correct Bearer secret must authenticate"
        );
    }

    /// Every `resolve_secret` failure used to collapse into one bare `None`;
    /// this proves each of the six is now its own, distinguishable variant
    /// rather than the same catch-all reached by six different paths.
    mod resolve_secret_reasons {
        use super::*;
        use wheel_core::{EndpointConfig, HttpMethod, Node, Position, ResponseMode, VaultConfig};

        /// A fresh vault + endpoint pair, wired, with no value stored yet —
        /// the shared starting point every variant test edits from.
        fn wired(state: &AppState) -> (rusqlite::Connection, Node, MatchedEndpoint) {
            let conn = crate::db::open_memory().unwrap();
            let vault = Node::new(
                Uuid::new_v4(),
                "v".parse().unwrap(),
                Position::default(),
                NodeConfig::Vault(VaultConfig {
                    keys: vec!["S".into()],
                }),
            );
            let endpoint = Node::new(
                Uuid::new_v4(),
                "e".parse().unwrap(),
                Position::default(),
                NodeConfig::Endpoint(EndpointConfig {
                    method: HttpMethod::Post,
                    path: "/hook".into(),
                    response_mode: ResponseMode::Ack,
                    auth: wheel_core::EndpointAuth::Bearer {
                        vault_ref: "v/S".into(),
                    },
                }),
            );
            board::create(&conn, &vault).unwrap();
            board::create(&conn, &endpoint).unwrap();
            board::add_wire(&conn, endpoint.id, vault.id, WireType::Read, None).unwrap();
            let matched = MatchedEndpoint {
                id: endpoint.id,
                name: endpoint.name.clone(),
                config: match &endpoint.config {
                    NodeConfig::Endpoint(c) => c.clone(),
                    _ => unreachable!(),
                },
            };
            let _ = state;
            (conn, vault, matched)
        }

        #[test]
        fn malformed_vault_ref_is_named_not_conflated_with_a_missing_vault() {
            let state = crate::api::test_state();
            let (conn, _vault, mut matched) = wired(&state);
            *state.db.lock().unwrap() = conn;
            matched.config.auth = wheel_core::EndpointAuth::Bearer {
                vault_ref: "no-slash-here".into(),
            };
            assert_eq!(
                resolve_secret(&state, &matched, "no-slash-here"),
                Err(SecretError::MalformedRef)
            );
        }

        #[test]
        fn no_wire_is_no_readable_vault_not_a_bare_none() {
            let state = crate::api::test_state();
            let conn = crate::db::open_memory().unwrap();
            let vault = Node::new(
                Uuid::new_v4(),
                "v".parse().unwrap(),
                Position::default(),
                NodeConfig::Vault(VaultConfig {
                    keys: vec!["S".into()],
                }),
            );
            let endpoint = Node::new(
                Uuid::new_v4(),
                "e".parse().unwrap(),
                Position::default(),
                NodeConfig::Endpoint(EndpointConfig {
                    method: HttpMethod::Post,
                    path: "/hook".into(),
                    response_mode: ResponseMode::Ack,
                    auth: wheel_core::EndpointAuth::Bearer {
                        vault_ref: "v/S".into(),
                    },
                }),
            );
            board::create(&conn, &vault).unwrap();
            board::create(&conn, &endpoint).unwrap();
            // No wire drawn at all -- the vault exists, but the endpoint has
            // no capability to read it.
            let vk = state.supervisor.require_vault_key().unwrap();
            crate::vault::put(&conn, vk, vault.id, "S", "x").unwrap();
            *state.db.lock().unwrap() = conn;
            let matched = MatchedEndpoint {
                id: endpoint.id,
                name: endpoint.name.clone(),
                config: match &endpoint.config {
                    NodeConfig::Endpoint(c) => c.clone(),
                    _ => unreachable!(),
                },
            };
            assert_eq!(
                resolve_secret(&state, &matched, "v/S"),
                Err(SecretError::NoReadableVault)
            );
        }

        #[test]
        fn a_wired_vault_with_nothing_stored_is_no_such_key_not_decrypt_failed() {
            let state = crate::api::test_state();
            let (conn, _vault, matched) = wired(&state);
            // Wired, but nothing was ever `put` at "S".
            *state.db.lock().unwrap() = conn;
            assert_eq!(
                resolve_secret(&state, &matched, "v/S"),
                Err(SecretError::NoSuchKey)
            );
        }

        #[test]
        fn no_vault_key_on_this_deployment_is_named_and_not_confused_with_the_others() {
            // Built by hand, not `test_state()`: this is the one axis
            // `test_state` hardcodes (a real vault key), so the "no key at
            // all" deployment shape needs its own AppState.
            let cfg = std::sync::Arc::new(crate::config::Config {
                project_id: Uuid::new_v4(),
                engine_secret: "0123456789abcdef".into(),
                vault_key: None,
                data_dir: std::env::temp_dir()
                    .join(format!("wheel-ingress-test-{}", Uuid::new_v4())),
                listen: wheel_core::ListenAddr::parse("tcp://127.0.0.1:7999").unwrap(),
                json_logs: false,
                tool_allow_hosts: Vec::new(),
                startup_deadline_secs: crate::config::DEFAULT_STARTUP_DEADLINE_SECS,
                harness_auth: crate::config::HarnessAuthPolicy::default(),
                script_execution_enabled: false,
            });
            let conn = crate::db::open_memory().unwrap();
            let vault = Node::new(
                Uuid::new_v4(),
                "v".parse().unwrap(),
                Position::default(),
                NodeConfig::Vault(VaultConfig {
                    keys: vec!["S".into()],
                }),
            );
            let endpoint = Node::new(
                Uuid::new_v4(),
                "e".parse().unwrap(),
                Position::default(),
                NodeConfig::Endpoint(EndpointConfig {
                    method: HttpMethod::Post,
                    path: "/hook".into(),
                    response_mode: ResponseMode::Ack,
                    auth: wheel_core::EndpointAuth::Bearer {
                        vault_ref: "v/S".into(),
                    },
                }),
            );
            board::create(&conn, &vault).unwrap();
            board::create(&conn, &endpoint).unwrap();
            board::add_wire(&conn, endpoint.id, vault.id, WireType::Read, None).unwrap();
            let db = std::sync::Arc::new(std::sync::Mutex::new(conn));
            let events = std::sync::Arc::new(crate::events::Bus::new());
            let supervisor =
                crate::supervisor::Supervisor::new(cfg.clone(), db.clone(), events.clone());
            let state = AppState {
                supervisor: std::sync::Arc::new(supervisor),
                cfg,
                db,
                events,
                logins: std::sync::Arc::new(crate::oauth::LoginSessions::default()),
                ingress_rate: std::sync::Arc::new(RateLimiter::default()),
            };
            let matched = MatchedEndpoint {
                id: endpoint.id,
                name: endpoint.name.clone(),
                config: match &endpoint.config {
                    NodeConfig::Endpoint(c) => c.clone(),
                    _ => unreachable!(),
                },
            };
            assert!(matches!(
                resolve_secret(&state, &matched, "v/S"),
                Err(SecretError::NoVaultKey(_))
            ));
        }

        /// The smoking-gun case: a value stored under one vault key that no
        /// longer decrypts under the key this engine is now running with (a
        /// `WHEEL_VAULT_KEY` rotated without re-encrypting stored values).
        /// Previously silent by construction (`vault::get`'s `Err` swallowed
        /// by `.ok()`) -- the exact gap PM asked this logging to close.
        #[test]
        fn a_value_that_wont_decrypt_under_the_current_key_is_named_not_swallowed() {
            let state = crate::api::test_state();
            let (conn, vault, matched) = wired(&state);
            let old_key = crate::vault::VaultKey::from_base64(&base64_encode([9u8; 32])).unwrap();
            crate::vault::put(&conn, &old_key, vault.id, "S", "x").unwrap();
            *state.db.lock().unwrap() = conn;
            // `state`'s own supervisor holds the [7u8; 32] key `test_state`
            // hardcodes -- different from `old_key` above, so the stored
            // ciphertext will not decrypt under it.
            assert_eq!(
                resolve_secret(&state, &matched, "v/S"),
                Err(SecretError::DecryptFailed)
            );
        }

        fn base64_encode(bytes: [u8; 32]) -> String {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(bytes)
        }
    }

    /// ADVERSARY, review of #103: a secret containing a URL-reserved byte,
    /// correctly percent-encoded per RFC 3986 by whoever built the `?token=`
    /// URL, must still match -- it did not, since the extraction never
    /// decoded it.
    #[test]
    fn percent_decode_reverses_a_correctly_encoded_query_value() {
        fn decode(s: &str) -> String {
            String::from_utf8(percent_decode_bytes(s)).unwrap()
        }
        assert_eq!(decode("abc"), "abc");
        assert_eq!(decode("a%2Bb%26c%25d"), "a+b&c%d");
        // `+` is a literal plus in a URI query component (RFC 3986), not a
        // form-encoded space -- unlike `application/x-www-form-urlencoded`.
        assert_eq!(decode("a+b"), "a+b");
        // A malformed escape passes through literally rather than being
        // dropped or panicking on a non-hex/short tail.
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("100%2"), "100%2");
        assert_eq!(decode("100%zz"), "100%zz");
    }

    /// The comparison-path fix itself (ADVERSARY, review of #103): a query
    /// value that percent-decodes to bytes which are NOT valid utf8 must
    /// never reach `constant_time_eq` as a lossy-substituted string. It must
    /// instead behave as "no credential presented" -- still a 401, never a
    /// panic, and never a many-to-one comparison.
    #[test]
    fn a_query_value_that_decodes_to_invalid_utf8_is_never_lossily_compared() {
        // %FF%FE is not valid utf8 in any interpretation.
        let bytes = percent_decode_bytes("%FF%FE");
        assert!(String::from_utf8(bytes).is_err());
    }

    /// The end-to-end version of the test above: a secret with a `&` in it,
    /// sent the way a real client would (percent-encoded in the URL), must
    /// authenticate through the real function, not just the helper.
    #[test]
    fn a_percent_encoded_query_token_authenticates_through_the_real_function() {
        use wheel_core::{
            AgentConfig, EndpointAuth, EndpointConfig, HttpMethod, Node, Position, ResponseMode,
            VaultConfig,
        };

        let state = crate::api::test_state();
        let conn = state.db.lock().unwrap();
        let vault = Node::new(
            Uuid::new_v4(),
            "v".parse().unwrap(),
            Position::default(),
            NodeConfig::Vault(VaultConfig {
                keys: vec!["S".into()],
            }),
        );
        let agent = Node::new(
            Uuid::new_v4(),
            "a".parse().unwrap(),
            Position::default(),
            NodeConfig::Agent(AgentConfig::default()),
        );
        let endpoint = Node::new(
            Uuid::new_v4(),
            "e".parse().unwrap(),
            Position::default(),
            NodeConfig::Endpoint(EndpointConfig {
                method: HttpMethod::Post,
                path: "/hook".into(),
                response_mode: ResponseMode::Ack,
                auth: EndpointAuth::Bearer {
                    vault_ref: "v/S".into(),
                },
            }),
        );
        board::create(&conn, &vault).unwrap();
        board::create(&conn, &agent).unwrap();
        board::create(&conn, &endpoint).unwrap();
        board::add_wire(&conn, endpoint.id, vault.id, WireType::Read, None).unwrap();
        board::add_wire(&conn, endpoint.id, agent.id, WireType::Send, None).unwrap();
        let vk = state.supervisor.require_vault_key().unwrap();
        // The secret itself contains `&` and `%`, both URL-reserved.
        crate::vault::put(&conn, vk, vault.id, "S", "a&b%c").unwrap();
        drop(conn);

        let matched = MatchedEndpoint {
            id: endpoint.id,
            name: endpoint.name.clone(),
            config: match &endpoint.config {
                NodeConfig::Endpoint(c) => c.clone(),
                _ => unreachable!(),
            },
        };
        let headers = HeaderMap::new();
        let uri: Uri = "/hook?token=a%26b%25c".parse().unwrap();

        assert!(
            authenticate(&state, &matched, &headers, &uri, b""),
            "a correctly percent-encoded query token must decode and match"
        );
    }

    /// ADVERSARY's coverage gap on the test above: proves the WIRING, not
    /// just `percent_decode_bytes` in isolation. A query token that decodes
    /// to invalid utf8 must be refused through the real `authenticate()`
    /// call. The stored secret is deliberately set to the EXACT string
    /// `String::from_utf8_lossy` would have produced from the presented
    /// bytes -- under the lossy bug this PR fixed, that made the two sides
    /// equal and authenticated the request. A secret that merely differs
    /// from the presented garbage would pass even with the bug still in
    /// place (any two different strings fail to match either way), so it
    /// would prove nothing; this is the one setup where the two behaviours
    /// actually diverge.
    #[test]
    fn a_query_token_decoding_to_invalid_utf8_is_refused_not_panicked_or_lossily_matched() {
        use wheel_core::{
            AgentConfig, EndpointAuth, EndpointConfig, HttpMethod, Node, Position, ResponseMode,
            VaultConfig,
        };

        let state = crate::api::test_state();
        let conn = state.db.lock().unwrap();
        let vault = Node::new(
            Uuid::new_v4(),
            "v".parse().unwrap(),
            Position::default(),
            NodeConfig::Vault(VaultConfig {
                keys: vec!["S".into()],
            }),
        );
        let agent = Node::new(
            Uuid::new_v4(),
            "a".parse().unwrap(),
            Position::default(),
            NodeConfig::Agent(AgentConfig::default()),
        );
        let endpoint = Node::new(
            Uuid::new_v4(),
            "e".parse().unwrap(),
            Position::default(),
            NodeConfig::Endpoint(EndpointConfig {
                method: HttpMethod::Post,
                path: "/hook".into(),
                response_mode: ResponseMode::Ack,
                auth: EndpointAuth::Bearer {
                    vault_ref: "v/S".into(),
                },
            }),
        );
        board::create(&conn, &vault).unwrap();
        board::create(&conn, &agent).unwrap();
        board::create(&conn, &endpoint).unwrap();
        board::add_wire(&conn, endpoint.id, vault.id, WireType::Read, None).unwrap();
        board::add_wire(&conn, endpoint.id, agent.id, WireType::Send, None).unwrap();
        let vk = state.supervisor.require_vault_key().unwrap();
        // The would-be collision: exactly what the OLD, lossy code would
        // have decoded "%FF%FE" into. If the fix regressed to lossy
        // conversion, this secret would equal the presented value and
        // authenticate would wrongly return true.
        let lossy_collision = String::from_utf8_lossy(&[0xFF, 0xFE]).into_owned();
        crate::vault::put(&conn, vk, vault.id, "S", &lossy_collision).unwrap();
        drop(conn);

        let matched = MatchedEndpoint {
            id: endpoint.id,
            name: endpoint.name.clone(),
            config: match &endpoint.config {
                NodeConfig::Endpoint(c) => c.clone(),
                _ => unreachable!(),
            },
        };
        let headers = HeaderMap::new();
        let uri: Uri = "/hook?token=%FF%FE".parse().unwrap();

        assert!(
            !authenticate(&state, &matched, &headers, &uri, b""),
            "a query token that decodes to invalid utf8 must be refused even when the stored \
             secret happens to equal its lossy decoding -- never panic, never lossily match"
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
    ///
    /// This does NOT exercise the real call site (QA's audit,
    /// reports/qa-audit-ingress-body-cap-blind-test-2026-09-12): `MAX_INGRESS_BODY`
    /// is DEFINED as `wheel_core::MAX_MESSAGE_BODY`, so the two can never
    /// disagree and this cannot observe a regression at `to_bytes` below.
    /// `an_oversized_body_is_rejected_before_it_is_buffered` is the test that
    /// actually proves the cap holds.
    #[test]
    fn the_body_cap_is_the_message_limit() {
        assert_eq!(MAX_INGRESS_BODY, wheel_core::MAX_MESSAGE_BODY);
    }

    fn endpoint_node(state: &AppState, path: &str) -> uuid::Uuid {
        let ep = wheel_core::Node::new(
            Uuid::new_v4(),
            "hook".parse().unwrap(),
            wheel_core::Position::default(),
            NodeConfig::Endpoint(wheel_core::EndpointConfig {
                method: HttpMethod::Post,
                path: path.into(),
                response_mode: wheel_core::ResponseMode::Ack,
                auth: wheel_core::EndpointAuth::None,
            }),
        );
        let conn = state.db.lock().unwrap();
        board::create(&conn, &ep).unwrap();
        ep.id
    }

    /// ADVERSARY 031, "size before signature": an unsigned body must never be
    /// buffered past the cap before HMAC verification runs, or a sender pays
    /// nothing to cost this project memory. Mutation-checked: with the cap at
    /// `to_bytes` disabled (or widened), this test is the one that goes red —
    /// `the_body_cap_is_the_message_limit` above stays green either way,
    /// because it never calls `handle`.
    #[tokio::test]
    async fn an_oversized_body_is_rejected_before_it_is_buffered() {
        let state = crate::api::test_state();
        endpoint_node(&state, "/hook");

        let oversized = vec![b'a'; MAX_INGRESS_BODY + 1];
        let resp = handle(
            State(state),
            Method::POST,
            "/hook".parse().unwrap(),
            HeaderMap::new(),
            Body::from(oversized),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// The boundary: a body AT the cap is not itself oversized. Without this,
    /// a fix for the case above that shaves the ceiling by one byte (an
    /// off-by-one on `to_bytes`'s limit) would pass unnoticed.
    #[tokio::test]
    async fn a_body_exactly_at_the_cap_is_accepted() {
        let state = crate::api::test_state();
        endpoint_node(&state, "/hook");

        let at_limit = vec![b'a'; MAX_INGRESS_BODY];
        let resp = handle(
            State(state),
            Method::POST,
            "/hook".parse().unwrap(),
            HeaderMap::new(),
            Body::from(at_limit),
        )
        .await;
        assert_ne!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
