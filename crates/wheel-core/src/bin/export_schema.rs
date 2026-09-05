//! Exports JSON Schema for every type Web and QA consume, into `docs/schema/`.
//!
//! Usage: `cargo run -p wheel-core --bin export-schema -- [out_dir]`
//! (default out dir `docs/schema`). QA's contract test regenerates and diffs
//! this, so it must be deterministic.

use std::{
    fs,
    path::{Path, PathBuf},
};

use schemars::{schema_for, JsonSchema};

fn write<T: JsonSchema>(dir: &Path, name: &str) -> std::io::Result<()> {
    let schema = schema_for!(T);
    let mut json = serde_json::to_string_pretty(&schema).expect("schema serializes");
    json.push('\n');
    let path = dir.join(format!("{name}.json"));
    fs::write(&path, json)?;
    println!("wrote {}", path.display());
    Ok(())
}

fn main() -> std::io::Result<()> {
    let dir: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "docs/schema".to_string())
        .into();
    fs::create_dir_all(&dir)?;

    use wheel_core::*;
    // The board and its parts — what Web renders.
    write::<Node>(&dir, "node")?;
    write::<NodeWithState>(&dir, "node-with-state")?;
    write::<NodeConfig>(&dir, "node-config")?;
    write::<NodeType>(&dir, "node-type")?;
    write::<Wire>(&dir, "wire")?;
    write::<WireSpec>(&dir, "wire-spec")?;
    write::<WireType>(&dir, "wire-type")?;
    write::<Position>(&dir, "position")?;
    // Runtime.
    write::<AgentState>(&dir, "agent-state")?;
    write::<NodeState>(&dir, "node-state")?;
    write::<AuthBegin>(&dir, "auth-begin")?;
    write::<AuthStatus>(&dir, "auth-status")?;
    write::<Message>(&dir, "message")?;
    write::<Event>(&dir, "event")?;
    write::<LogLine>(&dir, "log-line")?;
    // Host / API surface.
    write::<Capabilities>(&dir, "capabilities")?;
    write::<HostHealth>(&dir, "host-health")?;
    write::<SandboxInfo>(&dir, "sandbox-info")?;
    write::<SandboxUpsert>(&dir, "sandbox-upsert")?;
    write::<ErrorBody>(&dir, "error-body")?;

    // A machine-readable dump of the wire matrix, so Web can gate its wire
    // popover and QA can enumerate cells without transcribing the table again.
    let allowed: Vec<serde_json::Value> = allowed_wires()
        .into_iter()
        .map(|(f, t, w)| {
            serde_json::json!({ "from": f.as_str(), "to": t.as_str(), "type": w.as_str() })
        })
        .collect();
    let matrix = serde_json::json!({
        "$comment": "Generated from wheel_core::wire_allowed. Default DENY: any triple absent from `allowed` is rejected.",
        "node_types": NodeType::ALL.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
        "wire_types": WireType::ALL.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
        "allowed": allowed,
    });
    let path = dir.join("wire-matrix.json");
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&matrix).unwrap()),
    )?;
    println!("wrote {}", path.display());

    Ok(())
}
