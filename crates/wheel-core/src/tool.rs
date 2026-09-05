//! `tool` nodes: imported HTTP specs exposed to agents as callable tools (§3d).
//!
//! The engine is the only parser of OpenAPI/Swagger/Postman/Insomnia documents;
//! these are the *normalized* types everything downstream works with.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Methods a tool operation may use. Deliberately a separate enum from
/// [`crate::node::HttpMethod`]: `endpoint` nodes are contractually limited to
/// GET/POST/PUT/DELETE (§3), while imported specs routinely contain PATCH and
/// HEAD. Sharing one enum would silently widen the endpoint contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum ToolMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl ToolMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolMethod::Get => "GET",
            ToolMethod::Post => "POST",
            ToolMethod::Put => "PUT",
            ToolMethod::Patch => "PATCH",
            ToolMethod::Delete => "DELETE",
            ToolMethod::Head => "HEAD",
            ToolMethod::Options => "OPTIONS",
        }
    }
}

/// The document format a tool node was imported from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolFormat {
    Openapi3,
    Swagger2,
    Postman21,
    Insomnia4,
}

/// Where a parameter goes in the HTTP request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ParamLocation {
    Header,
    Path,
    Query,
    Cookie,
    Body,
}

/// How a field is filled when the operation is called (§3d).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum FillMode {
    /// The agent supplies it; appears in the operation's input schema.
    #[default]
    Agent,
    /// A value the operator typed. Never shown to the agent.
    Static,
    /// Resolved at call time from a wired vault. Never shown to the agent and
    /// never returned by `/v1/board`.
    Vault,
    /// Omitted from the request entirely.
    Hidden,
}

/// How one field is filled. `value`/`vault_ref` are meaningful only for their
/// corresponding mode; [`crate::validate::validate_config`] enforces that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct Fill {
    pub mode: FillMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// `<vault node name>/<key>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_ref: Option<String>,
}

impl Fill {
    pub fn agent() -> Self {
        Self::default()
    }

    /// Is this field visible to the agent? Only `agent`-mode fields are exposed
    /// in `wheel tool ls` and in the MCP input schema (§3d rule 1).
    pub fn is_agent_visible(&self) -> bool {
        self.mode == FillMode::Agent
    }

    /// Split a `vault_ref` into `(vault node name, key)`.
    pub fn parse_vault_ref(r: &str) -> Option<(&str, &str)> {
        let (v, k) = r.split_once('/')?;
        if v.is_empty() || k.is_empty() || k.contains('/') {
            return None;
        }
        Some((v, k))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolParam {
    pub name: String,
    pub location: ParamLocation,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema fragment for this field, used to build the MCP input schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
    #[serde(default)]
    pub fill: Fill,
}

/// One callable operation. Exposed over MCP as `<tool name>__<id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolOperation {
    /// Stable identifier, unique within the node. Charset is restricted because
    /// it is concatenated into an MCP tool name.
    pub id: String,
    pub method: ToolMethod,
    /// Path template relative to `base_url`, e.g. `/users/{id}`.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub params: Vec<ToolParam>,
}

fn default_true() -> bool {
    true
}

impl ToolOperation {
    /// The MCP tool name for this operation on a node called `tool_name`.
    pub fn mcp_name(&self, tool_name: &str) -> String {
        format!("{tool_name}__{}", self.id)
    }

    /// Fields the agent may supply — everything else is rejected at call time
    /// (§3d rules 1 and 2: static/vault are authoritative and an agent can
    /// never override them).
    pub fn agent_params(&self) -> impl Iterator<Item = &ToolParam> {
        self.params.iter().filter(|p| p.fill.is_agent_visible())
    }

    /// Vault refs this operation needs, for checking the `tool → vault` wire.
    pub fn vault_refs(&self) -> impl Iterator<Item = &str> {
        self.params.iter().filter_map(|p| {
            if p.fill.mode == FillMode::Vault {
                p.fill.vault_ref.as_deref()
            } else {
                None
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolConfig {
    /// Absolute `http(s)` origin every operation is resolved against.
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_format: Option<ToolFormat>,
    #[serde(default)]
    pub operations: Vec<ToolOperation>,
}

/// Hosts an outbound tool call may never reach (§3d rule 4, SSRF policy).
///
/// This catches literal addresses and known-internal suffixes without doing
/// DNS. The engine must ALSO re-check after resolution and after every
/// redirect, because a public name can resolve to a private address — this
/// function is a fast pre-filter, not the whole control.
pub fn host_is_denied(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let h = h
        .strip_prefix('[')
        .unwrap_or(&h)
        .strip_suffix(']')
        .unwrap_or({
            let s: &str = &h;
            s
        });

    if h.is_empty()
        || h == "localhost"
        || h.ends_with(".localhost")
        || h.ends_with(".internal")
        || h.ends_with(".local")
    {
        return true;
    }

    if let Ok(ip) = h.parse::<std::net::IpAddr>() {
        return ip_is_denied(ip);
    }
    false
}

/// Non-public IP ranges. `is_global` is unstable, so the checks are explicit.
pub fn ip_is_denied(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || o[0] == 0
                // 100.64.0.0/10 carrier-grade NAT
                || (o[0] == 100 && (64..128).contains(&o[1]))
                // 192.0.0.0/24 IETF protocol assignments
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                // 198.18.0.0/15 benchmarking
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
                // 240.0.0.0/4 reserved
                || o[0] >= 240
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                // unique local fc00::/7
                || (s[0] & 0xfe00) == 0xfc00
                // link local fe80::/10
                || (s[0] & 0xffc0) == 0xfe80
                // IPv4-mapped: check the embedded v4 address
                || v6.to_ipv4_mapped().is_some_and(|v4| ip_is_denied(IpAddr::V4(v4)))
        }
    }
}
