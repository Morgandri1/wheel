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
    export(&dir)
}

/// Write every exported schema into `dir`.
///
/// Split out of `main` so a test can run the real export into a temp dir and
/// compare it with what is committed: a type added to `wheel-core` but never
/// exported is invisible to Web until something fails at runtime, which is
/// how `CredentialKind` went missing once already.
fn export(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;

    use wheel_core::*;
    // The board and its parts — what Web renders.
    write::<Node>(dir, "node")?;
    write::<NodeWithState>(dir, "node-with-state")?;
    write::<NodeConfig>(dir, "node-config")?;
    write::<NodeType>(dir, "node-type")?;
    write::<Wire>(dir, "wire")?;
    write::<WireSpec>(dir, "wire-spec")?;
    write::<WireType>(dir, "wire-type")?;
    write::<Position>(dir, "position")?;
    write::<ToolConfig>(dir, "tool-config")?;
    write::<ToolOperation>(dir, "tool-operation")?;
    write::<ToolSource>(dir, "tool-source")?;
    write::<EndpointAuth>(dir, "endpoint-auth")?;
    // Runtime.
    write::<AgentState>(dir, "agent-state")?;
    write::<NodeState>(dir, "node-state")?;
    write::<AuthBegin>(dir, "auth-begin")?;
    write::<AuthStatus>(dir, "auth-status")?;
    write::<Message>(dir, "message")?;
    write::<Event>(dir, "event")?;
    write::<LogLine>(dir, "log-line")?;
    // Host / API surface.
    write::<Capabilities>(dir, "capabilities")?;
    write::<HostHealth>(dir, "host-health")?;
    write::<SandboxInfo>(dir, "sandbox-info")?;
    write::<SandboxUpsert>(dir, "sandbox-upsert")?;
    write::<ErrorBody>(dir, "error-body")?;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn committed_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/schema")
            .canonicalize()
            .expect("docs/schema must exist")
    }

    /// The export must produce exactly the files that are committed — no more,
    /// no fewer. A type added to `wheel-core` and forgotten here is invisible
    /// to Web until it breaks at runtime; a stale file left behind describes a
    /// type that no longer exists.
    #[test]
    fn the_export_matches_what_is_committed_in_docs_schema() {
        let tmp = std::env::temp_dir().join(format!("wheel-schema-{}", std::process::id()));
        fs::remove_dir_all(&tmp).ok();
        export(&tmp).unwrap();

        let names = |d: &Path| {
            let mut v: Vec<String> = fs::read_dir(d)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".json"))
                .collect();
            v.sort();
            v
        };
        assert_eq!(
            names(&tmp),
            names(&committed_dir()),
            "docs/schema is out of date -- run `cargo run -p wheel-core --bin export-schema -- docs/schema`"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    /// QA's contract test regenerates and diffs this, so two runs must produce
    /// identical bytes.
    #[test]
    fn the_export_is_deterministic_and_valid_json() {
        let base = std::env::temp_dir().join(format!("wheel-schema-det-{}", std::process::id()));
        let (a, b) = (base.join("a"), base.join("b"));
        fs::remove_dir_all(&base).ok();
        export(&a).unwrap();
        export(&b).unwrap();

        let mut checked = 0;
        for entry in fs::read_dir(&a).unwrap() {
            let name = entry.unwrap().file_name();
            let (x, y) = (
                fs::read_to_string(a.join(&name)).unwrap(),
                fs::read_to_string(b.join(&name)).unwrap(),
            );
            assert_eq!(x, y, "{name:?} is not deterministic");
            serde_json::from_str::<serde_json::Value>(&x)
                .unwrap_or_else(|e| panic!("{name:?} is not valid JSON: {e}"));
            assert!(x.ends_with('\n'), "{name:?} must end with a newline");
            checked += 1;
        }
        assert!(checked > 20, "expected the full schema set, got {checked}");
        fs::remove_dir_all(&base).ok();
    }

    /// The matrix dump is what Web gates its wire popover on and what QA
    /// enumerates, so it has to carry every triple the engine would allow --
    /// not a transcription of the table.
    #[test]
    fn the_wire_matrix_dump_agrees_with_the_function_that_enforces_it() {
        let tmp = std::env::temp_dir().join(format!("wheel-schema-wm-{}", std::process::id()));
        fs::remove_dir_all(&tmp).ok();
        export(&tmp).unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(tmp.join("wire-matrix.json")).unwrap())
                .unwrap();
        let allowed = v["allowed"].as_array().unwrap();
        assert_eq!(allowed.len(), wheel_core::allowed_wires().len());
        assert!(!allowed.is_empty());

        // Every listed triple really is allowed, and the count matches, so
        // nothing allowed is missing either.
        for cell in allowed {
            let f = cell["from"].as_str().unwrap();
            let t = cell["to"].as_str().unwrap();
            let w = cell["type"].as_str().unwrap();
            let found = wheel_core::allowed_wires()
                .into_iter()
                .any(|(a, b, c)| a.as_str() == f && b.as_str() == t && c.as_str() == w);
            assert!(found, "{f} -> {t} ({w}) is in the dump but not allowed");
        }
        fs::remove_dir_all(&tmp).ok();
    }
}
