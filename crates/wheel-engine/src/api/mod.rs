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
    /// Logins waiting for a pasted code. Each holds a live child process, so
    /// this is state with a cost and a TTL, not a cache.
    pub logins: Arc<crate::oauth::LoginSessions>,
}

/// An error that renders as the uniform `{"error":{"code","message"}}` body.
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
            // The request named something the storage layer cannot represent
            // (a table node whose name is not a sqlite identifier), which the
            // caller can fix by choosing a different name.
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

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
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
        .route("/inbox", get(cli_routes::inbox));

    Router::new()
        .route("/healthz", get(healthz))
        .nest("/v1", v1)
        .nest("/v1/cli", cli)
        .with_state(state)
}

/// Unauthenticated readiness probe. The host waits for this before reporting a
/// sandbox `running`, so it must answer as soon as the database is usable.
async fn healthz() -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true }))
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

    #[test]
    fn constant_time_eq_matches_normal_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        // Length mismatch must not panic or index out of range.
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
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
