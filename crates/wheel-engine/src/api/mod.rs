//! The engine control plane (`docs/PROTOCOL.md` §2).
//!
//! Three disjoint auth realms share one router:
//!   * `/healthz`   — none. The host's readiness probe.
//!   * `/v1/*`      — the engine secret, held only by the host.
//!   * `/v1/cli/*`  — a per-node token; never the engine secret.
//!
//! Keeping them disjoint is the point: a child process that somehow reached the
//! control-plane port still cannot use its own token there.

use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use rusqlite::Connection;
use serde::Deserialize;
use wheel_core::{ErrorBody, NodeConfig, NodeName, Position};

use crate::{config::Config, db};

pub mod agent_routes;
pub mod board_routes;
pub mod cli_routes;
pub mod events_route;
pub mod ingress;
mod table_routes;
pub mod tool_routes;
pub mod vault_routes;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    /// Owns every agent's child process. The ONLY thing that writes to a
    /// child's stdin (§3c#12).
    pub supervisor: Arc<crate::supervisor::Supervisor>,
    /// One writer connection: sqlite serialises writes anyway, and a single
    /// writer keeps the delivery loop's state transitions trivially correct.
    pub db: Arc<Mutex<Connection>>,
    /// Fan-out for `/v1/events`. Publishing never blocks, so a slow browser
    /// cannot stall the supervisor.
    pub events: Arc<crate::events::Bus>,
    /// Per-caller ingress rate limit. On a public URL this and the body cap
    /// are the only cost control, so it lives beside the state the handler
    /// already has rather than in a lazy static nothing can reset.
    pub ingress_rate: Arc<crate::api::ingress::RateLimiter>,
    /// Logins waiting for a pasted code. Each holds a live child process, so
    /// this is state with a cost and a TTL, not a cache.
    pub logins: Arc<crate::oauth::LoginSessions>,
}

/// An error that renders as the uniform `{"error":{"code","message"}}` body.
#[derive(Debug)]
pub struct ApiError(StatusCode, &'static str, String);

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, msg: impl Into<String>) -> Self {
        Self(status, code, msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", msg)
    }
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid", msg)
    }
    /// The engine cannot do this because of how it was STARTED, not because
    /// of anything in the request. 503 rather than 500 so a provisioning gap
    /// is not read as an engine bug — and the message names the variable.
    pub fn config(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "config", msg)
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", msg)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorBody::new(self.1, self.2))).into_response()
    }
}

impl From<db::board::BoardError> for ApiError {
    fn from(e: db::board::BoardError) -> Self {
        use db::board::BoardError as B;
        match e {
            B::NotFound(m) => ApiError::not_found(m),
            B::NameTaken(n) => ApiError::new(
                StatusCode::CONFLICT,
                "name_taken",
                format!("a node named {n:?} already exists"),
            ),
            // A denied wire is a policy answer, not a malformed request, so it
            // is 403 rather than 400 — and it is surfaced, never silent.
            B::Wire(w) => ApiError::new(StatusCode::FORBIDDEN, "wire_denied", w.to_string()),
            // 409: the request is well-formed and the wire is legal; the
            // BOARD is the thing that cannot accept it.
            B::Ambiguous(m) => ApiError::new(StatusCode::CONFLICT, "ambiguous_credential", m),
            B::Config(c) => ApiError::invalid(c.to_string()),
            // Almost always a name that cannot become a sqlite identifier,
            // and the message says which character and what to use instead --
            // so it is the caller's to fix, not an internal fault.
            B::Storage(m) => ApiError::invalid(m),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// Bearer check for `/v1/*`. Constant-time comparison, because this is the
/// entire control-plane boundary and a timing oracle on it is worth avoiding
/// even behind a private network.
async fn require_engine_secret(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    if !constant_time_eq(presented.as_bytes(), state.cfg.engine_secret.as_bytes()) {
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid engine secret",
        )
        .into_response();
    }
    next.run(req).await
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn router(state: AppState) -> Router {
    let v1 = Router::new()
        .route("/board", get(board_routes::get_board))
        .route("/nodes", post(board_routes::create_node))
        .route(
            "/nodes/{id}",
            axum::routing::patch(board_routes::patch_node).delete(board_routes::delete_node),
        )
        .route("/wires", post(board_routes::add_wire))
        .route("/wires", delete(board_routes::remove_wire))
        .route("/agents/{id}/start", post(agent_routes::start))
        .route("/agents/{id}/stop", post(agent_routes::stop))
        .route("/agents/{id}/restart", post(agent_routes::restart))
        .route("/agents/{id}/clear", post(agent_routes::clear))
        .route("/agents/{id}/send", post(agent_routes::send))
        .route("/agents/{id}/log", get(agent_routes::log))
        .route("/agents/{id}/inbox", get(agent_routes::inbox))
        .route(
            "/agents/{id}/inbox/{message_id}",
            get(agent_routes::inbox_one),
        )
        .route(
            "/agents/{id}/auth",
            get(agent_routes::auth_status).delete(agent_routes::auth_clear),
        )
        .route(
            "/agents/{id}/auth/complete",
            post(agent_routes::auth_complete),
        )
        .route("/agents/{id}/auth/begin", post(agent_routes::auth_begin))
        .route("/vault/{id}", get(vault_routes::list_keys))
        .route(
            "/vault/{id}/{key}",
            axum::routing::put(vault_routes::put_value).delete(vault_routes::delete_value),
        )
        .route("/tables/{id}/rows", get(table_routes::rows))
        .route("/tables/{id}/query", post(table_routes::query))
        .route("/tools/import", post(tool_routes::preview))
        .route("/tools/{id}/import", post(tool_routes::reimport))
        .route("/tools/{id}/ops", get(tool_routes::ops))
        .route("/tools/{id}/call", post(tool_routes::call))
        .route("/events", get(events_route::events_ws))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_engine_secret,
        ));

    // A separate realm: node tokens, never the engine secret. Nested outside
    // the engine-secret route_layer on purpose — a child holding its own token
    // must not be able to reach /v1/board, and the engine secret must not work
    // here either.
    let cli = Router::new()
        .route("/whoami", get(cli_routes::whoami))
        .route("/connections", get(cli_routes::connections))
        .route("/ls", get(cli_routes::ls))
        .route("/list", get(cli_routes::list))
        .route("/read", get(cli_routes::read))
        .route("/secret", get(cli_routes::secret_get))
        .route("/secret/keys", get(cli_routes::secret_keys))
        .route("/write", post(cli_routes::write))
        .route("/msg", post(cli_routes::msg))
        .route("/inbox", get(cli_routes::inbox))
        .route("/rm", post(cli_routes::rm))
        .route("/query", post(cli_routes::query))
        .route(
            "/tool",
            get(cli_routes::tool_ls).post(cli_routes::tool_call),
        )
        .route("/ctx/clear", post(cli_routes::ctx_clear))
        .route("/mcp/tools", get(cli_routes::mcp_tools));

    Router::new()
        .route("/healthz", get(healthz))
        .nest("/v1", v1)
        .nest("/v1/cli", cli)
        // PUBLIC, and the only public surface: mounted outside the
        // engine-secret layer because a webhook provider has nothing but a URL.
        .nest("/ingress", ingress::router())
        .with_state(state)
}

/// Unauthenticated readiness probe. The host waits for this before reporting a
/// sandbox `running`, so it must answer as soon as the database is usable.
///
/// It also reports STALLED agents — ones holding messages queued longer than
/// the startup deadline while not being transitional. Three times in one night
/// this engine was up, answering 200, with delivery not happening underneath:
/// a delivery task that unwound and vanished, an ingress path that enqueued and
/// never pumped, and an ephemeral agent parked in `starting` for ever. In every
/// case /healthz said the same word it says when everything works.
///
/// `ok` STAYS TRUE when agents are stalled, deliberately. The docker
/// healthcheck restarts the container on failure, so reporting a stalled agent
/// as unhealthy would restart the whole engine — killing every OTHER tenant's
/// agents because one of them is stuck. That is the same isolation argument
/// that made a poison message quarantine rather than take the process down.
/// This makes the condition VISIBLE; it does not make it fatal.
///
/// Computed on demand rather than watched: the engine must idle at ~0 CPU (§2),
/// so there is no background poll here.
async fn healthz(State(s): State<AppState>) -> impl IntoResponse {
    // Taken BEFORE the db lock and awaited outside it: the agents map is behind
    // an async mutex and the db lock is a std one, so holding the second across
    // an await on the first is how a deadlock gets written.
    let live = s.supervisor.live_agents().await;
    let stalled = match s.db.lock() {
        Ok(conn) => stalled_agents(&conn, s.cfg.startup_deadline_secs as i64, &live),
        // A poisoned lock is worth saying nothing about rather than guessing.
        Err(_) => Vec::new(),
    };

    if !stalled.is_empty() {
        tracing::warn!(
            stalled = stalled.len(),
            "agents are holding messages nothing is delivering"
        );
    }
    Json(serde_json::json!({
        "ok": true,
        "stalled": stalled,
        "version": env!("CARGO_PKG_VERSION"),
        "build": build_id(),
    }))
}

/// Agents holding work that nothing is coming for.
///
/// Split out of the handler so the DECISION is testable without an HTTP
/// harness: which agents are named, and — the part that matters more — which
/// are not. A stall report that names healthy agents is one an operator learns
/// to scroll past, which is the failure it exists to prevent.
fn stalled_agents(
    conn: &rusqlite::Connection,
    deadline_secs: i64,
    live: &std::collections::HashSet<uuid::Uuid>,
) -> Vec<serde_json::Value> {
    // A `delivered` row with no live process is a WEDGE, not a turn. Nothing on
    // boot returns it to `queued`, so the agent can never move again on its own
    // -- and the age-based report below deliberately excludes `delivered` rows,
    // which is right for a live turn and hides this completely. Of the silent
    // failures we have found, this is the only one where the health signal
    // denies a state the system cannot leave on its own (QA's framing).
    let mut out: Vec<serde_json::Value> = crate::db::messages::agents_holding_delivered(conn)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(agent, behind)| {
            let id = uuid::Uuid::parse_str(&agent).ok()?;
            if live.contains(&id) {
                return None;
            }
            Some(serde_json::json!({
                "agent": id,
                "queued": behind,
                "reason": "wedged: a message was delivered and no process is running it"
            }))
        })
        .collect();
    let wedged: std::collections::HashSet<String> = out
        .iter()
        .filter_map(|v| v.get("agent").and_then(|a| a.as_str()).map(String::from))
        .collect();

    out.extend(
        crate::db::messages::agents_with_work_older_than(conn, deadline_secs)
        .unwrap_or_default()
        .into_iter()
        .filter(|(agent, _)| {
            // Transitional agents are 041's business, and an agent that is
            // parked, stopped, unauthenticated or out of budget is not FAILING
            // to deliver — it is not delivering, which is a different thing and
            // not a fault to report.
            !matches!(
                crate::db::board::agent_state(conn, *agent)
                    .unwrap_or_default()
                    .status,
                wheel_core::AgentStatus::Starting
                    | wheel_core::AgentStatus::Parked
                    | wheel_core::AgentStatus::Stopped
                    | wheel_core::AgentStatus::NeedsAuth
                    | wheel_core::AgentStatus::BudgetExhausted
            )
        })
        .filter(|(agent, _)| !wedged.contains(&agent.to_string()))
        .map(|(agent, n)| {
            serde_json::json!({ "agent": agent, "queued": n, "reason": "queued longer than the deadline" })
        }),
    );
    out
}

/// What code this RUNNING engine actually is.
///
/// Four times in one night someone had to answer "is the thing running the
/// thing we merged?" and could only infer it: a stale `wheel-engine:test` tag
/// gave a PASS describing a different binary, a CI run described whoever pushed
/// last rather than the commit in question, and a `make check` described a
/// working tree rather than HEAD. Every one is the same question — which input
/// produced this result — and the engine could not answer it about itself.
///
/// COMPILE-TIME, not an environment variable, and that is not a preference. The
/// `process` backend — which is what production runs on Railway — spawns the
/// engine with `env_clear()` plus a deliberate allowlist, so a runtime env
/// stamp arrives empty and the engine reports `unknown` on exactly the
/// deployment where the answer matters. I shipped the env version, then watched
/// a live process-backend engine report `unknown` from a correctly stamped
/// image, which is the failure this whole field exists to prevent — a confirm
/// that quietly tells you nothing.
///
/// Baked into the binary it survives `env_clear`, cannot be set by whatever
/// spawned the process, and travels with the artefact rather than beside it.
/// `unknown` when nothing stamped the build, which is honest: an unstamped
/// build is exactly where an operator must conclude nothing.
fn build_id() -> &'static str {
    option_env!("WHEEL_BUILD_SHA").unwrap_or("unknown")
}

// --- request bodies --------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateNode {
    pub name: NodeName,
    #[serde(flatten)]
    pub config: NodeConfig,
    #[serde(default)]
    pub position: Position,
}

#[derive(Debug, Deserialize)]
pub struct PatchNode {
    #[serde(default)]
    pub name: Option<NodeName>,
    #[serde(default)]
    pub position: Option<Position>,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The way IN: a client mid-drag still sends floats, and anything past the
    /// bound clamps rather than being refused (PM's ruling: a 400 mid-drag is
    /// not an improvement on a node that springs back).
    #[test]
    fn a_patched_position_is_rounded_and_clamped_before_the_handler_sees_it() {
        let p: PatchNode =
            serde_json::from_value(serde_json::json!({"position": {"x": 10.6, "y": -99999.0}}))
                .unwrap();
        let pos = p.position.expect("position survives the partial patch");
        assert_eq!(pos.x, 11, "10.6 rounds to the nearest cell");
        assert_eq!(pos.y, i16::MIN, "past the bound it clamps");

        let c: CreateNode = serde_json::from_value(serde_json::json!({
            "name": "n", "type": "ctx", "config": {"markdown": ""},
            "position": {"x": 99999.0, "y": 0.5}
        }))
        .unwrap();
        assert_eq!(c.position.x, i16::MAX);
        assert_eq!(c.position.y, 1);
    }

    /// Web renders the position from the PATCH RESPONSE rather than from what
    /// it sent, which is what makes a clamp invisible to the operator instead
    /// of a drag that silently stops. That only holds while the handler answers
    /// with the full node: a 204, or a re-read from the database instead of the
    /// value it just stored, puts Web back on its fallback without anything
    /// failing here.
    ///
    /// Checked in the source because the failure is an ABSENCE — no assertion
    /// about a returned body can notice a handler that stopped returning one.
    #[test]
    fn patch_answers_with_the_node_it_stored_not_an_empty_body() {
        let src = include_str!("board_routes.rs");
        // The WHOLE function, not a fixed line window. The first version of
        // this read 6 lines for the signature and 40 for the body, and broke
        // the moment the handler grew — flagging a contract that still held.
        // A gate that fails when the code merely MOVES teaches people to edit
        // the gate, which is how a gate stops meaning anything.
        let body = src
            .split("pub async fn patch_node")
            .nth(1)
            .and_then(|rest| rest.split("\npub ").next())
            .expect("patch_node exists");

        assert!(
            body.contains("ApiResult<Json<Node>>"),
            "patch_node must answer with the full node; Web reads `position` off this reply"
        );
        assert!(
            body.contains("board::update_with(&conn, &node") && body.contains("Ok(Json(node))"),
            "patch_node must return the SAME node value it stored, so the reply carries the \
             clamped position rather than an echo of the request"
        );
    }

    /// The stall report's value is in what it does NOT say. Every state below
    /// is a healthy agent that a naive "has old queued work" check would name,
    /// and each false positive is a step toward an operator ignoring the field.
    #[test]
    fn a_stall_report_names_only_agents_nothing_is_coming_for() {
        use wheel_core::AgentStatus;
        let conn = crate::db::open_memory().unwrap();

        let agent = |name: &str| {
            let n = wheel_core::Node::new(
                uuid::Uuid::new_v4(),
                name.parse().unwrap(),
                wheel_core::Position::default(),
                wheel_core::NodeConfig::Agent(wheel_core::AgentConfig::default()),
            );
            crate::db::board::create(&conn, &n).unwrap();
            n.id
        };
        let age = |id: uuid::Uuid| {
            conn.execute(
                "UPDATE messages SET created_at = datetime('now', '-600 seconds') WHERE to_id = ?1",
                [id.to_string()],
            )
            .unwrap();
        };
        let queue = |id: uuid::Uuid| {
            crate::db::messages::enqueue(
                &conn,
                wheel_core::MessageSender::User,
                id,
                "work".into(),
                None,
            )
            .unwrap()
        };

        // The one that SHOULD be named: old work, agent alive and not busy.
        let stuck = agent("stuck");
        queue(stuck);
        age(stuck);
        crate::db::board::set_status(&conn, stuck, AgentStatus::Idle, None);

        // Fresh work is in flight, not stranded.
        let fresh = agent("fresh");
        queue(fresh);
        crate::db::board::set_status(&conn, fresh, AgentStatus::Idle, None);

        // Parked with old work is 041's business and the resume path's, not a
        // stall: nothing is wrong, nothing is running.
        let parked = agent("parked");
        queue(parked);
        age(parked);
        crate::db::board::set_status(&conn, parked, AgentStatus::Parked, None);

        let named: Vec<String> = stalled_agents(&conn, 60, &Default::default())
            .into_iter()
            .filter_map(|v| v.get("agent").and_then(|a| a.as_str()).map(str::to_string))
            .collect();

        assert_eq!(
            named,
            vec![stuck.to_string()],
            "only the agent with old work and a live, unbusy session may be named; naming a fresh \
             or parked one teaches the operator to ignore the field"
        );

        // A `delivered` row with NO live process is a wedge: nothing on boot
        // requeues it, so the agent can never move again on its own. Before
        // this, the age report excluded `delivered` rows and /healthz said
        // nothing at all.
        let wedged = agent("wedged");
        queue(wedged);
        crate::db::board::set_status(&conn, wedged, AgentStatus::Running, None);
        conn.execute(
            "UPDATE messages SET state = 'delivered' WHERE to_id = ?1",
            rusqlite::params![wedged.to_string()],
        )
        .unwrap();

        let dead: Vec<serde_json::Value> = stalled_agents(&conn, 60, &Default::default());
        let named_dead: Vec<String> = dead
            .iter()
            .filter_map(|v| v.get("agent").and_then(|a| a.as_str()).map(str::to_string))
            .collect();
        assert!(
            named_dead.contains(&wedged.to_string()),
            "an agent holding a delivered message with no process is permanently stuck and MUST be \
             reported; got {named_dead:?}"
        );
        assert!(
            dead.iter().any(|v| v
                .get("reason")
                .and_then(|r| r.as_str())
                .is_some_and(|r| r.contains("wedged"))),
            "the report must say WHY, or the operator cannot tell a wedge from a slow queue"
        );

        // ...and with a live process it is an ordinary turn in progress, which
        // is the commonest healthy state on the board. Reporting that would cry
        // wolf and teach everyone to ignore the field.
        let live: std::collections::HashSet<uuid::Uuid> = [wedged].into_iter().collect();
        let alive: Vec<String> = stalled_agents(&conn, 60, &live)
            .into_iter()
            .filter_map(|v| v.get("agent").and_then(|a| a.as_str()).map(str::to_string))
            .collect();
        assert!(
            !alive.contains(&wedged.to_string()),
            "a delivered message on a LIVE agent is a turn in progress, not a wedge"
        );
    }

    /// The engine must be able to say what code it IS.
    ///
    /// Four times in one night someone had to answer "is the thing running the
    /// thing we merged?" and could only infer it — a stale image tag, a CI run
    /// describing a later push, a `make check` describing a working tree. The
    /// engine could not answer it about itself, so every answer was an
    /// inference from behaviour.
    ///
    /// The assertion that carries this is the UNSTAMPED case. A build with no
    /// commit stamped must say `unknown`, not a version number that reads as an
    /// answer — an operator who is told something specific will believe it, and
    /// the whole point is to stop people concluding from the wrong input.
    #[test]
    fn an_unstamped_build_says_so_rather_than_offering_a_number() {
        // The image sets WHEEL_BUILD_SHA; a cargo build does not, which is
        // exactly the case under test.
        if std::env::var("WHEEL_BUILD_SHA").is_err() {
            assert_eq!(
                build_id(),
                "unknown",
                "an unstamped build must not present a number an operator would trust"
            );
        }
        // And the crate version is compile-time, so it is always truthful.
        assert!(
            !env!("CARGO_PKG_VERSION").is_empty(),
            "the version is stamped at compile time and cannot be missing"
        );
    }

    #[test]
    fn constant_time_eq_matches_normal_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        // Length mismatch must not panic or index out of range.
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }

    /// ADVERSARY 025. `tool_ls` and `tool_call` existed, compiled, were unit
    /// tested, and were never registered — so the operator path worked, every
    /// test passed, and every AGENT got a 404. Nothing in the type system
    /// notices a handler nobody routed.
    ///
    /// I made this mistake with a text edit that silently matched nothing, and
    /// then read a grep that returned empty and carried on. This is the check
    /// that would have caught both.
    #[test]
    fn every_cli_handler_is_actually_routed() {
        let handlers = include_str!("cli_routes.rs");
        let router = include_str!("mod.rs");

        let mut unrouted = Vec::new();
        for line in handlers.lines() {
            let Some(rest) = line.trim().strip_prefix("pub async fn ") else {
                continue;
            };
            let name = rest.split('(').next().unwrap_or_default().trim();
            if name.is_empty() {
                continue;
            }
            if !router.contains(&format!("cli_routes::{name}")) {
                unrouted.push(name.to_string());
            }
        }
        assert!(
            unrouted.is_empty(),
            "these handlers exist but no route reaches them: {unrouted:?}"
        );
    }

    /// The other direction: the CLI grammar in PROTOCOL.md is a contract, and
    /// a command whose route was never built is a command that 404s in the
    /// agent's hands while the docs promise it works.
    #[test]
    fn every_documented_cli_route_exists() {
        let router = include_str!("mod.rs");
        for path in [
            "/whoami",
            "/connections",
            "/ls",
            "/list",
            "/read",
            "/secret",
            "/write",
            "/msg",
            "/inbox",
            "/rm",
            "/query",
            "/tool",
            "/ctx/clear",
            "/mcp/tools",
        ] {
            assert!(
                router.contains(&format!("\"{path}\"")),
                "PROTOCOL documents {path} but nothing routes it"
            );
        }
    }

    /// A project spawned without its vault key is a provisioning gap in the
    /// caller, not a fault in this engine. It answered 500 `internal` once,
    /// and was duly debugged as an engine bug.
    #[test]
    fn a_missing_vault_key_is_a_503_that_names_the_variable() {
        let ApiError(status, code, message) = ApiError::config(crate::supervisor::NO_VAULT_KEY);
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "config");
        assert!(
            message.contains("WHEEL_VAULT_KEY"),
            "whoever reads this has to know which variable to set: {message}"
        );
    }

    /// A key that is present but malformed has a different fix from one that
    /// was never set, so the two must not collapse into one message.
    #[test]
    fn an_unusable_vault_key_reads_differently_from_a_missing_one() {
        assert_ne!(
            crate::supervisor::NO_VAULT_KEY,
            crate::supervisor::BAD_VAULT_KEY
        );
        assert!(crate::supervisor::BAD_VAULT_KEY.contains("base64"));
    }
}
