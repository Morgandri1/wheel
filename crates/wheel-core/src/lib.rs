//! # wheel-core
//!
//! Canonical shared types for Wheel. **This crate is the source of truth**: the
//! engine, the host, the CLI and the API all depend on it, and the web UI
//! consumes the JSON Schema exported from it
//! (`cargo run -p wheel-core --bin export-schema`).
//!
//! Everything here is pure data + pure functions — no I/O, no async, no
//! database, so it stays cheap to depend on and exhaustively testable.
//!
//! The shapes are fixed by `docs/ARCHITECTURE.md` §3/§4/§4b. If you change a
//! JSON shape here you must regenerate `docs/schema/` in the same commit and
//! tell PM, because Web regenerates its TypeScript types from it.

pub mod event;
pub mod host;
pub mod message;
pub mod name;
pub mod node;
pub mod preamble;
pub mod spawn;
pub mod state;
pub mod timestamp;
pub mod tool;
pub mod validate;
pub mod wire;

pub use event::{Event, LogLine, LogStream, WireDenial};
pub use host::{
    Capabilities, ErrorBody, ErrorDetail, HostHealth, SandboxBackend, SandboxInfo, SandboxStatus,
    SandboxUpsert,
};
pub use message::{
    escape_envelope_body, sha256_hex, Message, MessageReceipt, MessageSender, MessageState,
    MAX_MESSAGE_BODY,
};
pub use name::{validate_name, Ident, NameError, NodeName, NAME_MAX_LEN, RESERVED_NAMES};
pub use node::{
    AgentConfig, Budget, ChestConfig, Column, ColumnType, CtxConfig, EndpointConfig, Harness,
    HttpMethod, McpConfig, McpTransport, Node, NodeConfig, NodeType, Position, ResponseMode,
    ScriptConfig, ScriptLanguage, TableConfig, VaultConfig, DEFAULT_IDLE_TIMEOUT_SECS,
    DEFAULT_SCRIPT_TIMEOUT_SECS,
};
pub use preamble::{compose_system_prompt, orchestration_block, PreambleInput, WireLine};
pub use spawn::{ListenAddr, ListenAddrError};
pub use state::{
    AgentState, AgentStatus, AuthBegin, AuthMode, AuthStatus, NodeState, NodeWithState, Spend,
};
pub use timestamp::Timestamp;
pub use tool::{
    host_is_denied, ip_is_denied, Fill, FillMode, ParamLocation, ToolConfig, ToolFormat,
    ToolMethod, ToolOperation, ToolParam,
};
pub use validate::{normalize_chest_key, validate_config, validate_endpoint_path, ConfigError};
pub use wire::{allowed_wires, check_wire, wire_allowed, Wire, WireError, WireSpec, WireType};

/// The implicit primary-key column every `table` node has, in addition to its
/// configured columns (ARCHITECTURE.md §3: "Table nodes therefore always have
/// an implicit primary key column `key TEXT`").
///
/// `wheel write <table>/<row>` upserts by this key. User configs may not
/// declare a column with this name — [`validate::validate_config`] rejects it.
pub const TABLE_KEY_COLUMN: &str = "key";

/// Engine control-plane port in `docker` sandbox mode. In `process` mode the
/// engine listens on a unix socket instead and this is unused.
pub const ENGINE_PORT: u16 = 7000;

/// Host API port (private network only).
pub const HOST_PORT: u16 = 7100;

/// Exit code the `wheel` CLI returns when a call is denied because the caller's
/// node has no wire granting it. Mirrors `yoke`, which agents already know.
pub const EXIT_WIRE_DENIED: i32 = 3;

/// Exit code for "the node you named does not exist".
pub const EXIT_NOT_FOUND: i32 = 4;

// ---------------------------------------------------------------------------
// Limits (ARCHITECTURE.md §3c#6). Documented in PROTOCOL.md and enforced by
// the CLI/MCP tools *before* sending, so callers get a clear error instead of
// discovering a limit by failing. The engine re-checks: never trust the child.
// ---------------------------------------------------------------------------

/// Max `ctx` markdown / table row value.
pub const MAX_VALUE_BYTES: usize = 1024 * 1024;

/// Max chest blob.
pub const MAX_BLOB_BYTES: usize = 50 * 1024 * 1024;

/// Max bytes captured from a script run's stdout/stderr.
pub const MAX_SCRIPT_OUTPUT_BYTES: usize = 1024 * 1024;
