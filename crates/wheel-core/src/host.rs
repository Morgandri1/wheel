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

/// How tenants are kept apart on one machine.
///
/// The contract's isolation story is a uid per project (§2, §5b): the data dir
/// is 0700 to that uid and the engine socket is only openable by it. Anything
/// else is a different product with the same API, so it is named rather than
/// left as an absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum UidIsolation {
    /// One unix uid per project. The supported posture.
    #[default]
    PerProject,
    /// Every project runs as the SAME uid, because setuid was unavailable.
    /// A laptop convenience; never a deployment.
    Shared,
}

/// Opt IN to running every project as one uid. Never inferred.
///
/// Deliberately an opt-in rather than a fallback when setuid fails: a
/// production host that silently drops to a shared uid after a permissions
/// change would keep serving, keep looking healthy, and have no isolation —
/// and nothing in its logs would be alarming enough to notice.
pub const ENV_ALLOW_SHARED_UID: &str = "WHEEL_ALLOW_SHARED_UID";

/// What a shared uid actually gives up, in the words an operator needs.
///
/// Specific on purpose. "Reduced isolation" tells nobody anything; this says
/// which boundary is gone and what an agent can therefore reach.
pub const SHARED_UID_WARNING: &str = concat!(
    "running in shared-uid mode: every project runs as THIS user, so the ",
    "per-project boundary does not exist. Any agent can read any project's ",
    "data directory, open any project's engine socket, and read any other ",
    "child's environment. Vault values are protected only by the encryption ",
    "key, which lives in that same environment. Use this for one local user ",
    "on a machine you trust, never for a deployment serving anyone else."
);

impl UidIsolation {
    /// What the environment asks for. `Shared` only when explicitly requested.
    pub fn from_env() -> Self {
        match std::env::var(ENV_ALLOW_SHARED_UID).ok().as_deref() {
            Some("1") | Some("true") | Some("yes") => Self::Shared,
            _ => Self::PerProject,
        }
    }

    pub fn is_shared(self) -> bool {
        self == Self::Shared
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PerProject => "per_project",
            Self::Shared => "shared",
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Shared-uid mode is opt-IN. A deployment that lost setuid must fail or
    /// warn, never quietly serve every tenant as one user.
    #[test]
    fn a_shared_uid_is_never_inferred_only_requested() {
        // The default is the supported posture, whatever the machine can do.
        assert_eq!(UidIsolation::default(), UidIsolation::PerProject);
        assert!(!UidIsolation::default().is_shared());
        assert_eq!(UidIsolation::PerProject.as_str(), "per_project");
        assert_eq!(UidIsolation::Shared.as_str(), "shared");
        assert!(UidIsolation::Shared.is_shared());
    }

    /// The warning has to name the boundary that is gone. "Reduced isolation"
    /// tells an operator nothing they can act on.
    #[test]
    fn the_shared_uid_warning_says_what_is_actually_lost() {
        let w = SHARED_UID_WARNING;
        for must_mention in ["data directory", "engine socket", "environment", "never"] {
            assert!(
                w.contains(must_mention),
                "warning must mention {must_mention:?}: {w}"
            );
        }
        assert_eq!(ENV_ALLOW_SHARED_UID, "WHEEL_ALLOW_SHARED_UID");
    }

    /// The host API is a contract between three crates and QA. These spellings
    /// are what API sends and Web reads, so they are pinned rather than
    /// assumed.
    #[test]
    fn sandbox_status_and_backend_serialize_lowercase() {
        assert_eq!(
            serde_json::to_string(&SandboxBackend::Docker).unwrap(),
            "\"docker\""
        );
        assert_eq!(
            serde_json::to_string(&SandboxBackend::Process).unwrap(),
            "\"process\""
        );
        for (s, want) in [
            (SandboxStatus::Stopped, "stopped"),
            (SandboxStatus::Starting, "starting"),
            (SandboxStatus::Running, "running"),
            (SandboxStatus::Error, "error"),
        ] {
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{want}\""));
            assert_eq!(s.as_str(), want);
            assert_eq!(s.to_string(), want);
            // as_str and serde must never drift apart.
            assert_eq!(
                serde_json::from_str::<SandboxStatus>(&format!("\"{want}\"")).unwrap(),
                s
            );
        }
    }

    /// A sandbox nobody has started is `stopped`, not `running`: the default
    /// has to be the safe end of that axis.
    #[test]
    fn a_sandbox_defaults_to_stopped_and_no_capabilities() {
        assert_eq!(SandboxStatus::default(), SandboxStatus::Stopped);
        assert!(!Capabilities::default().http, "ingress is off by default");
    }

    /// Public ingress is served only when `http` is true, so a payload that
    /// omits it must read as disabled rather than fail or default open.
    #[test]
    fn omitted_capabilities_read_as_disabled() {
        let c: Capabilities = serde_json::from_str("{}").unwrap();
        assert!(!c.http);
        let up: SandboxUpsert =
            serde_json::from_str(r#"{"engine_secret":"s","vault_key":"k"}"#).unwrap();
        assert!(!up.capabilities.http);
        assert_eq!(up.engine_secret, "s");
        assert_eq!(up.vault_key, "k");
    }

    #[test]
    fn sandbox_info_omits_absent_fields_rather_than_sending_null() {
        let info = SandboxInfo {
            id: Uuid::nil(),
            status: SandboxStatus::Running,
            last_error: None,
            started_at: None,
            capabilities: Capabilities { http: true },
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("last_error"), "{json}");
        assert!(!json.contains("started_at"), "{json}");
        assert!(json.contains("\"status\":\"running\""), "{json}");
        assert!(json.contains("\"http\":true"), "{json}");

        // ...and it survives the trip back.
        let back: SandboxInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, info);
    }

    #[test]
    fn an_error_body_has_the_uniform_shape_both_services_render() {
        let e = ErrorBody::new("wire_denied", "no wire from a to b (need: write)");
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"error":{"code":"wire_denied","message":"no wire from a to b (need: write)"}}"#
        );
        let back: ErrorBody =
            serde_json::from_str(r#"{"error":{"code":"not_found","message":"gone"}}"#).unwrap();
        assert_eq!(back.error.code, "not_found");
        assert_eq!(back.error.message, "gone");
    }

    #[test]
    fn host_health_reports_the_backend_it_is_actually_running() {
        let h = HostHealth {
            ok: true,
            sandbox_backend: SandboxBackend::Process,
            projects_running: 3,
        };
        let json = serde_json::to_string(&h).unwrap();
        assert!(json.contains("\"sandbox_backend\":\"process\""), "{json}");
        assert_eq!(serde_json::from_str::<HostHealth>(&json).unwrap(), h);
    }
}
