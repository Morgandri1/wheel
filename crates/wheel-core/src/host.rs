//! Types for the host API (ARCHITECTURE.md §4b) and the sandbox abstraction.
//!
//! `wheel-host` runs on the single big engine machine and owns every project's
//! sandbox. These types live in `wheel-core` because the API builds requests
//! against them and QA asserts on them; the `Sandbox` trait itself lives in
//! `wheel-host` (it needs async and backend deps).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::timestamp::Timestamp;

/// Which sandbox implementation the host is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SandboxBackend {
    /// Local dev / any VM with a docker daemon: one container per project,
    /// engine reachable over TCP on the container network.
    Docker,
    /// Production (Railway): one unix uid per project, engine reachable only
    /// over a unix socket owned by that uid.
    Process,
}

/// Lifecycle of a project's sandbox. Mirrors `Project.status` in §5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum SandboxStatus {
    #[default]
    Stopped,
    Starting,
    Running,
    Error,
}

impl SandboxStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SandboxStatus::Stopped => "stopped",
            SandboxStatus::Starting => "starting",
            SandboxStatus::Running => "running",
            SandboxStatus::Error => "error",
        }
    }
}

impl std::fmt::Display for SandboxStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-project capabilities, toggled by the owner through the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct Capabilities {
    /// Public ingress at `/p/<project_id>/*` is served only when this is true.
    #[serde(default)]
    pub http: bool,
}

/// `GET /host/v1/healthz`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HostHealth {
    pub ok: bool,
    pub sandbox_backend: SandboxBackend,
    pub projects_running: u32,
}

/// `PUT /host/v1/projects/:id` — idempotent create-or-update of a sandbox
/// record. The API holds these secrets encrypted in Postgres and hands them to
/// the host here; the host is the only process that has them at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SandboxUpsert {
    /// Bearer the host must present to this project's engine control plane.
    pub engine_secret: String,
    /// Base64 per-project key the engine uses to encrypt vault values at rest.
    pub vault_key: String,
    #[serde(default)]
    pub capabilities: Capabilities,
}

/// `GET /host/v1/projects/:id`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SandboxInfo {
    pub id: Uuid,
    pub status: SandboxStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    #[serde(default)]
    pub capabilities: Capabilities,
}

/// The uniform error body used by both the host and the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorDetail {
    /// Stable machine-readable code, e.g. `wire_denied`, `not_found`.
    pub code: String,
    pub message: String,
}

impl ErrorBody {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: ErrorDetail {
                code: code.into(),
                message: message.into(),
            },
        }
    }
}
