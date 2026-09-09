// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Every file under `web/public/workflow_templates` must be a legal, applicable board.
//!
//! `docs/proposals/wow-templates.md` §3: a template only ever targets a freshly created, EMPTY
//! project, so everything the live `board/apply` call would refuse is knowable from the file alone
//! — no engine, no project, no network call. That makes "malformed template" a build-time fact
//! rather than something a user discovers by clicking "use template" and watching it fail. This is
//! the gate: it reads every template file and fails the build the moment one of them would not
//! survive a real apply, or would create a node the engine itself would refuse.
//!
//! Three layers, same order the real apply path would hit them:
//! 1. Parses as the template envelope (title/description/requires_capabilities/board).
//! 2. Every node's name and config pass the same pure checks the engine runs on create
//!    (`validate_name`/`validate_table_name`, `validate_config_with`) — PM's fold-in: a bad node
//!    name or an SSRF-denied tool `base_url` must fail here, not at first live instantiate.
//! 3. The board as a whole passes `apply::validate` against an empty existing board — duplicate
//!    names, unknown wire targets, self-wires, and anything the wire matrix forbids.
//!
//! A fourth, template-specific check: an endpoint node's presence implies the board needs public
//! HTTP, so `requires_capabilities.http` must say so — a template that ships an endpoint and
//! forgets to declare it would otherwise instantiate silently non-functional (the endpoint node
//! exists, ingress 403s until the operator finds the toggle themselves).

use serde::Deserialize;
use std::path::{Path, PathBuf};
use wheel_api::apply::{validate, ApplyPolicy, EmittedBoard, ExistingBoard};
use wheel_core::{validate_config_with, validate_name, validate_table_name, NodeType};

#[derive(Debug, Deserialize)]
struct TemplateFile {
    title: String,
    description: String,
    #[serde(default)]
    requires_capabilities: RequiresCapabilities,
    board: EmittedBoard,
}

#[derive(Debug, Default, Deserialize)]
struct RequiresCapabilities {
    #[serde(default)]
    http: bool,
}

fn templates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/public/workflow_templates")
}

/// Every failure mode a template file can hit, named so a CI failure says exactly what to fix and
/// in which file — a panic message alone would still have to be read, but should not have to be
/// decoded.
fn check_template(path: &Path, raw: &str) -> Result<(), String> {
    let file: TemplateFile = serde_json::from_str(raw)
        .map_err(|e| format!("{}: not a valid template file: {e}", path.display()))?;

    if file.title.trim().is_empty() {
        return Err(format!("{}: title must not be empty", path.display()));
    }
    if file.description.trim().is_empty() {
        return Err(format!("{}: description must not be empty", path.display()));
    }

    let mut has_endpoint = false;
    for node in &file.board.nodes {
        let node_type = node.config.node_type();
        if node_type == NodeType::Table {
            validate_table_name(&node.name).map_err(|e| {
                format!(
                    "{}: node {:?} has an invalid table name: {e}",
                    path.display(),
                    node.name
                )
            })?;
        } else {
            validate_name(&node.name).map_err(|e| {
                format!(
                    "{}: node {:?} has an invalid name: {e}",
                    path.display(),
                    node.name
                )
            })?;
        }
        // No SSRF allowlist here on purpose: production boots with none (the engine refuses to
        // start with `WHEEL_TOOL_ALLOW_HOST` set), so an empty allowlist is the only honest thing
        // for a template — the same allowlist a template author's own deploy will actually run
        // under, not a permissive one that would let a `tool` node's base_url pass the gate and
        // then be refused live.
        validate_config_with(&node.config, &[]).map_err(|e| {
            format!(
                "{}: node {:?} has an invalid config: {e}",
                path.display(),
                node.name
            )
        })?;
        if node_type == NodeType::Endpoint {
            has_endpoint = true;
        }
    }

    if has_endpoint && !file.requires_capabilities.http {
        return Err(format!(
            "{}: board has an endpoint node but requires_capabilities.http is not true — \
             instantiating this template would leave a public-looking endpoint 403ing until \
             someone finds the capability toggle themselves",
            path.display()
        ));
    }

    validate(
        &file.board,
        &ExistingBoard::default(),
        ApplyPolicy::default(),
    )
    .map_err(|refusals| {
        let reasons: Vec<String> = refusals.iter().map(|r| r.message()).collect();
        format!(
            "{}: board refused by apply::validate: {}",
            path.display(),
            reasons.join("; ")
        )
    })?;

    Ok(())
}

#[test]
fn every_shipped_template_is_a_legal_applicable_board() {
    let dir = templates_dir();
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("could not read {}: {e}", dir.display()));

    let mut checked = 0usize;
    let mut failures = Vec::new();
    for entry in entries {
        let entry = entry.expect("readable dir entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
        if let Err(msg) = check_template(&path, &raw) {
            failures.push(msg);
        }
        checked += 1;
    }

    // A gate that silently checked zero files would pass forever regardless of what ships. If the
    // directory is ever legitimately empty, delete this test with it — an empty gate proving
    // nothing is worse than no gate, because it reads as coverage that is not there.
    assert!(
        checked > 0,
        "no template files found under {}",
        dir.display()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

mod negative {
    //! Proves the gate actually rejects what it claims to, rather than trusting the positive test
    //! above to mean anything on its own (a check that has never failed proves nothing).

    use super::*;

    #[test]
    fn rejects_a_bad_node_name() {
        let raw = r#"{
            "title": "t", "description": "d",
            "board": { "nodes": [
                { "name": "Bad Name", "type": "ctx", "config": { "markdown": "x" } }
            ], "wires": [] }
        }"#;
        let err = check_template(Path::new("<fixture>"), raw).unwrap_err();
        assert!(err.contains("invalid name"), "{err}");
    }

    #[test]
    fn rejects_a_table_node_whose_name_cannot_be_an_sqlite_identifier() {
        let raw = r#"{
            "title": "t", "description": "d",
            "board": { "nodes": [
                { "name": "bad-table", "type": "table", "config": { "columns": [] } }
            ], "wires": [] }
        }"#;
        let err = check_template(Path::new("<fixture>"), raw).unwrap_err();
        assert!(err.contains("invalid table name"), "{err}");
    }

    #[test]
    fn rejects_an_illegal_wire() {
        let raw = r#"{
            "title": "t", "description": "d",
            "board": { "nodes": [
                { "name": "a", "type": "agent", "config": { "harness": "claude", "system_prompt": "hi" } },
                { "name": "v", "type": "vault", "config": { "keys": [] } }
            ], "wires": [ { "from": "v", "to": "a", "type": "read" } ] }
        }"#;
        let err = check_template(Path::new("<fixture>"), raw).unwrap_err();
        assert!(err.contains("refused by apply::validate"), "{err}");
    }

    #[test]
    fn rejects_a_wire_to_a_node_that_is_not_on_the_board() {
        let raw = r#"{
            "title": "t", "description": "d",
            "board": { "nodes": [
                { "name": "a", "type": "agent", "config": { "harness": "claude", "system_prompt": "hi" } }
            ], "wires": [ { "from": "a", "to": "ghost", "type": "send" } ] }
        }"#;
        let err = check_template(Path::new("<fixture>"), raw).unwrap_err();
        assert!(err.contains("refused by apply::validate"), "{err}");
    }

    #[test]
    fn rejects_an_endpoint_node_with_an_undeclared_http_capability() {
        let raw = r#"{
            "title": "t", "description": "d",
            "board": { "nodes": [
                { "name": "hook", "type": "endpoint",
                  "config": { "method": "POST", "path": "/hook", "response_mode": "ack" } }
            ], "wires": [] }
        }"#;
        let err = check_template(Path::new("<fixture>"), raw).unwrap_err();
        assert!(err.contains("requires_capabilities.http"), "{err}");
    }

    #[test]
    fn accepts_the_same_shape_once_the_capability_is_declared() {
        let raw = r#"{
            "title": "t", "description": "d",
            "requires_capabilities": { "http": true },
            "board": { "nodes": [
                { "name": "hook", "type": "endpoint",
                  "config": { "method": "POST", "path": "/hook", "response_mode": "ack" } }
            ], "wires": [] }
        }"#;
        check_template(Path::new("<fixture>"), raw).expect("a declared capability must pass");
    }

    #[test]
    fn rejects_malformed_json() {
        let err = check_template(Path::new("<fixture>"), "{ not json").unwrap_err();
        assert!(err.contains("not a valid template file"), "{err}");
    }

    #[test]
    fn rejects_an_empty_title() {
        let raw = r#"{
            "title": "  ", "description": "d",
            "board": { "nodes": [], "wires": [] }
        }"#;
        let err = check_template(Path::new("<fixture>"), raw).unwrap_err();
        assert!(err.contains("title must not be empty"), "{err}");
    }
}
