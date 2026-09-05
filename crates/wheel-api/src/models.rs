//! Persisted types. Shapes follow ARCHITECTURE.md §5.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ProjectStatus {
    Stopped,
    Starting,
    Running,
    Error,
}

impl ProjectStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectStatus::Stopped => "stopped",
            ProjectStatus::Starting => "starting",
            ProjectStatus::Running => "running",
            ProjectStatus::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// Whether the public ingress route `/p/<id>/*` is served for this project.
    ///
    /// Defaults to `false`, and `#[serde(default)]` means a missing or malformed field also lands
    /// on `false`. Anything reachable without authentication is opt-in, never opt-out — so the
    /// derived `Default` is load-bearing, not incidental.
    #[serde(default)]
    pub http: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub id: Uuid,
    pub owner_id: String,
    pub name: String,
    pub capabilities: Capabilities,
    pub status: ProjectStatus,
    /// Where this project's public ingress lives, per ARCHITECTURE §5. Derived from
    /// `PUBLIC_BASE_URL` rather than stored, so it stays correct if the deployment moves.
    /// Populated at the response boundary by `with_ingress_base`.
    pub ingress_base_url: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl Project {
    /// Fill in the ingress URL for a response. Kept separate from the row conversion because the
    /// public base URL is deployment configuration, not a property of the row.
    pub fn with_ingress_base(mut self, public_base_url: &str) -> Self {
        self.ingress_base_url = format!("{}/p/{}", public_base_url.trim_end_matches('/'), self.id);
        self
    }
}

/// Row shape as it comes out of postgres, before `capabilities` is parsed.
#[derive(sqlx::FromRow)]
pub struct ProjectRow {
    pub id: Uuid,
    pub owner_id: String,
    pub name: String,
    pub capabilities: serde_json::Value,
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<ProjectRow> for Project {
    fn from(r: ProjectRow) -> Self {
        Project {
            id: r.id,
            owner_id: r.owner_id,
            name: r.name,
            // A malformed capabilities blob must fail closed (no public ingress), not panic and
            // not default to open.
            capabilities: serde_json::from_value(r.capabilities).unwrap_or_default(),
            ingress_base_url: String::new(),
            status: match r.status.as_str() {
                "starting" => ProjectStatus::Starting,
                "running" => ProjectStatus::Running,
                "error" => ProjectStatus::Error,
                _ => ProjectStatus::Stopped,
            },
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// Project names are shown in a UI and used in log lines, never in shell commands, SQL text, or
/// Docker object names — those all key off the UUID. Validation is therefore about keeping the
/// product tidy, plus refusing control characters that would corrupt logs.
pub fn validate_project_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("name must not be empty".into());
    }
    // Count characters, not bytes: a 64-emoji name is fine, a 64-byte truncation is not.
    let len = trimmed.chars().count();
    if len > 64 {
        return Err("name must be at most 64 characters".into());
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err("name must not contain control characters".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(validate_project_name("my project").is_ok());
        assert!(validate_project_name("é".repeat(64).as_str()).is_ok());
        assert!(validate_project_name("").is_err());
        assert!(validate_project_name("   ").is_err());
        assert!(validate_project_name(&"a".repeat(65)).is_err());
        assert!(
            validate_project_name("bad\nname").is_err(),
            "control chars enable log forging"
        );
        assert!(validate_project_name("bad\u{0}name").is_err());
    }

    #[test]
    fn capabilities_fail_closed() {
        // Garbage in the jsonb column must not yield http: true.
        let row_val = serde_json::json!("not an object");
        let caps: Capabilities = serde_json::from_value(row_val).unwrap_or_default();
        assert!(!caps.http);
    }
}
