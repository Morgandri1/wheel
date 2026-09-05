//! Validation shared by the engine and the API so the two cannot disagree
//! about what a legal node config is.

use crate::{
    node::{EndpointConfig, NodeConfig, ScriptConfig, TableConfig},
    tool::{Fill, FillMode, ToolConfig},
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("endpoint path must start with '/'")]
    PathNotAbsolute,
    #[error("endpoint path must not contain '..'")]
    PathTraversal,
    #[error("endpoint path must not contain a query string or fragment")]
    PathHasQuery,
    #[error("endpoint path is too long (max {max})")]
    PathTooLong { max: usize },
    #[error("table must have at least one column")]
    NoColumns,
    #[error("duplicate column name {0:?}")]
    DuplicateColumn(String),
    #[error("column name {0:?} is reserved: every table has an implicit `key TEXT` primary key")]
    ReservedColumn(String),
    #[error("table may have at most {max} columns")]
    TooManyColumns { max: usize },
    #[error("script source must not be empty")]
    EmptyScript,
    #[error("script timeout must be between 1 and {max} seconds")]
    BadTimeout { max: u32 },
    #[error("mcp transport 'stdio' requires 'command'")]
    McpMissingCommand,
    #[error("mcp transport 'http' requires 'url'")]
    McpMissingUrl,
    #[error("mcp url must be http:// or https://")]
    McpBadUrl,
    #[error("duplicate vault key {0:?}")]
    DuplicateVaultKey(String),
    #[error("vault key {0:?} is not a valid environment variable name")]
    BadVaultKey(String),
    #[error("agent system_prompt is too long (max {max} bytes)")]
    SystemPromptTooLong { max: usize },
    #[error("tool base_url must be an absolute http:// or https:// URL")]
    ToolBadBaseUrl,
    #[error("tool base_url host {0:?} is not reachable: private, loopback or internal addresses are denied")]
    ToolDeniedHost(String),
    #[error("duplicate tool operation id {0:?}")]
    DuplicateOperation(String),
    #[error("tool operation id {0:?} must match ^[a-zA-Z0-9_][a-zA-Z0-9_-]*$")]
    BadOperationId(String),
    #[error("tool operation {0:?} path must start with '/'")]
    ToolBadPath(String),
    #[error("field {0:?} has fill mode 'static' but no value")]
    StaticFillMissingValue(String),
    #[error("field {0:?} has fill mode 'vault' but vault_ref is missing or not '<vault>/<key>'")]
    BadVaultRef(String),
}

pub const MAX_ENDPOINT_PATH: usize = 512;
pub const MAX_TABLE_COLUMNS: usize = 64;
pub const MAX_SCRIPT_TIMEOUT_SECS: u32 = 600;
pub const MAX_SYSTEM_PROMPT: usize = 128 * 1024;

/// Endpoint paths become public URLs (`/p/<project>/<path>`) and are matched
/// against incoming requests, so they must be unambiguous and must not escape
/// their prefix.
pub fn validate_endpoint_path(path: &str) -> Result<(), ConfigError> {
    if !path.starts_with('/') {
        return Err(ConfigError::PathNotAbsolute);
    }
    if path.len() > MAX_ENDPOINT_PATH {
        return Err(ConfigError::PathTooLong {
            max: MAX_ENDPOINT_PATH,
        });
    }
    if path.contains('?') || path.contains('#') {
        return Err(ConfigError::PathHasQuery);
    }
    // Reject `..` as a whole path segment. A literal `..` inside a longer
    // segment (e.g. `/a..b`) is harmless, but segment-wise traversal is not.
    if path.split('/').any(|seg| seg == "..") {
        return Err(ConfigError::PathTraversal);
    }
    Ok(())
}

fn validate_table(cfg: &TableConfig) -> Result<(), ConfigError> {
    if cfg.columns.is_empty() {
        return Err(ConfigError::NoColumns);
    }
    if cfg.columns.len() > MAX_TABLE_COLUMNS {
        return Err(ConfigError::TooManyColumns {
            max: MAX_TABLE_COLUMNS,
        });
    }
    let mut seen = std::collections::BTreeSet::new();
    for c in &cfg.columns {
        // Every table node has an implicit `key TEXT PRIMARY KEY` (§3), which
        // `wheel write <table>/<row>` upserts on. A user column of the same
        // name would collide in the generated DDL.
        if c.name.as_str() == crate::TABLE_KEY_COLUMN {
            return Err(ConfigError::ReservedColumn(c.name.to_string()));
        }
        if !seen.insert(c.name.as_str()) {
            return Err(ConfigError::DuplicateColumn(c.name.to_string()));
        }
    }
    Ok(())
}

fn validate_script(cfg: &ScriptConfig) -> Result<(), ConfigError> {
    if cfg.source.trim().is_empty() {
        return Err(ConfigError::EmptyScript);
    }
    let t = cfg.timeout_secs();
    if t == 0 || t > MAX_SCRIPT_TIMEOUT_SECS {
        return Err(ConfigError::BadTimeout {
            max: MAX_SCRIPT_TIMEOUT_SECS,
        });
    }
    Ok(())
}

fn validate_endpoint(cfg: &EndpointConfig) -> Result<(), ConfigError> {
    validate_endpoint_path(&cfg.path)
}

/// Vault keys are exported as environment variables into agent children, so
/// they must be legal env var names — otherwise they would either be silently
/// dropped or, worse, allow smuggling extra assignments.
fn validate_vault_key(key: &str) -> Result<(), ConfigError> {
    let ok = !key.is_empty()
        && !key.chars().next().unwrap().is_ascii_digit()
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(ConfigError::BadVaultKey(key.to_string()))
    }
}

/// Validate a node's config. Called by the engine on create and patch, and by
/// the API before it forwards.
pub fn validate_config(cfg: &NodeConfig) -> Result<(), ConfigError> {
    match cfg {
        NodeConfig::Agent(a) => {
            if a.system_prompt.len() > MAX_SYSTEM_PROMPT {
                return Err(ConfigError::SystemPromptTooLong {
                    max: MAX_SYSTEM_PROMPT,
                });
            }
            Ok(())
        }
        NodeConfig::Ctx(_) => Ok(()),
        NodeConfig::Table(t) => validate_table(t),
        NodeConfig::Endpoint(e) => validate_endpoint(e),
        NodeConfig::Script(s) => validate_script(s),
        NodeConfig::Mcp(m) => {
            use crate::node::McpTransport;
            match m.transport {
                McpTransport::Stdio => {
                    if m.command.as_deref().unwrap_or("").trim().is_empty() {
                        return Err(ConfigError::McpMissingCommand);
                    }
                }
                McpTransport::Http => {
                    let url = m.url.as_deref().unwrap_or("");
                    if url.trim().is_empty() {
                        return Err(ConfigError::McpMissingUrl);
                    }
                    if !(url.starts_with("http://") || url.starts_with("https://")) {
                        return Err(ConfigError::McpBadUrl);
                    }
                }
            }
            Ok(())
        }
        NodeConfig::Vault(v) => {
            let mut seen = std::collections::BTreeSet::new();
            for k in &v.keys {
                validate_vault_key(k)?;
                if !seen.insert(k.as_str()) {
                    return Err(ConfigError::DuplicateVaultKey(k.clone()));
                }
            }
            Ok(())
        }
        NodeConfig::Chest(_) => Ok(()),
        NodeConfig::Tool(t) => validate_tool(t),
    }
}

fn validate_fill(field: &str, fill: &Fill) -> Result<(), ConfigError> {
    match fill.mode {
        FillMode::Static => {
            if fill.value.is_none() {
                return Err(ConfigError::StaticFillMissingValue(field.to_string()));
            }
        }
        FillMode::Vault => {
            let ok = fill
                .vault_ref
                .as_deref()
                .and_then(Fill::parse_vault_ref)
                .is_some();
            if !ok {
                return Err(ConfigError::BadVaultRef(field.to_string()));
            }
        }
        FillMode::Agent | FillMode::Hidden => {}
    }
    Ok(())
}

fn validate_tool(cfg: &ToolConfig) -> Result<(), ConfigError> {
    let rest = cfg
        .base_url
        .strip_prefix("https://")
        .or_else(|| cfg.base_url.strip_prefix("http://"))
        .ok_or(ConfigError::ToolBadBaseUrl)?;
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    // Strip a port, but not the colons of a bracketed IPv6 literal.
    let host_only = if host.starts_with('[') {
        host.split(']')
            .next()
            .unwrap_or(host)
            .trim_start_matches('[')
    } else {
        host.split(':').next().unwrap_or(host)
    };
    if host_only.is_empty() {
        return Err(ConfigError::ToolBadBaseUrl);
    }
    if crate::tool::host_is_denied(host_only) {
        return Err(ConfigError::ToolDeniedHost(host_only.to_string()));
    }

    let mut seen = std::collections::BTreeSet::new();
    for op in &cfg.operations {
        let id_ok = !op.id.is_empty()
            && op
                .id
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            && op
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !id_ok {
            return Err(ConfigError::BadOperationId(op.id.clone()));
        }
        if !seen.insert(op.id.as_str()) {
            return Err(ConfigError::DuplicateOperation(op.id.clone()));
        }
        if !op.path.starts_with('/') {
            return Err(ConfigError::ToolBadPath(op.id.clone()));
        }
        if op.path.split('/').any(|seg| seg == "..") {
            return Err(ConfigError::PathTraversal);
        }
        for p in &op.params {
            validate_fill(&format!("{}.{}", op.id, p.name), &p.fill)?;
        }
    }
    Ok(())
}

/// Normalise and validate a chest blob key: relative, no `..`, no absolute
/// paths, no backslashes, no empty or dot segments.
pub fn normalize_chest_key(key: &str) -> Result<String, ConfigError> {
    if key.starts_with('/') || key.contains('\\') || key.contains('\0') {
        return Err(ConfigError::PathTraversal);
    }
    let mut out: Vec<&str> = Vec::new();
    for seg in key.split('/') {
        match seg {
            "" | "." => continue,
            ".." => return Err(ConfigError::PathTraversal),
            s => out.push(s),
        }
    }
    if out.is_empty() {
        return Err(ConfigError::PathTraversal);
    }
    Ok(out.join("/"))
}
