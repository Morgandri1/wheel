//! §3d tool nodes: fill modes, agent visibility, and the SSRF pre-filter.

use serde_json::json;
use wheel_core::*;

fn op(id: &str, params: Vec<ToolParam>) -> ToolOperation {
    ToolOperation {
        id: id.into(),
        method: ToolMethod::Get,
        path: "/v1/things".into(),
        summary: Some("List things".into()),
        enabled: true,
        params,
    }
}

fn param(name: &str, fill: Fill) -> ToolParam {
    ToolParam {
        name: name.into(),
        location: ParamLocation::Query,
        required: false,
        description: None,
        schema: None,
        fill,
    }
}

fn cfg(base: &str, ops: Vec<ToolOperation>) -> NodeConfig {
    NodeConfig::Tool(ToolConfig {
        kind: ToolKind::Http,
        source: ToolSource {
            format: ToolFormat::Openapi,
            raw: "{}".into(),
            imported_at: Timestamp::parse_rfc3339("2026-09-05T00:00:00Z").unwrap(),
        },
        base_url: base.into(),
        operations: ops,
    })
}

#[test]
fn tool_node_round_trips_with_the_right_tag() {
    let c = cfg("https://api.example.com", vec![op("listThings", vec![])]);
    assert_eq!(c.node_type(), NodeType::Tool);
    let node = Node::new(
        uuid::Uuid::nil(),
        NodeName::new("petstore").unwrap(),
        Position::default(),
        c,
    );
    let v = serde_json::to_value(&node).unwrap();
    assert_eq!(v["type"], json!("tool"));
    assert_eq!(v["config"]["base_url"], json!("https://api.example.com"));
    let back: Node = serde_json::from_value(v).unwrap();
    assert_eq!(back, node);
}

/// §3d rule 1: only `agent`-mode fields are ever exposed to the agent.
#[test]
fn only_agent_mode_fields_are_visible_to_the_agent() {
    let o = op(
        "search",
        vec![
            param("q", Fill::agent()),
            param(
                "api_key",
                Fill {
                    mode: FillMode::Vault,
                    vault_ref: Some("secrets/API_KEY".into()),
                    ..Default::default()
                },
            ),
            param(
                "tenant",
                Fill {
                    mode: FillMode::Static,
                    value: Some("acme".into()),
                    ..Default::default()
                },
            ),
            param(
                "debug",
                Fill {
                    mode: FillMode::Hidden,
                    ..Default::default()
                },
            ),
        ],
    );
    let visible: Vec<_> = o.agent_params().map(|p| p.name.as_str()).collect();
    assert_eq!(
        visible,
        vec!["q"],
        "secrets and static values must not leak"
    );

    let refs: Vec<_> = o.vault_refs().collect();
    assert_eq!(refs, vec!["secrets/API_KEY"]);

    assert_eq!(o.mcp_name("petstore"), "petstore__search");
}

#[test]
fn fill_modes_are_validated_against_their_payload() {
    // static without a value is meaningless
    let c = cfg(
        "https://x.example",
        vec![op(
            "a",
            vec![param(
                "p",
                Fill {
                    mode: FillMode::Static,
                    ..Default::default()
                },
            )],
        )],
    );
    assert!(matches!(
        validate_config(&c),
        Err(ConfigError::StaticFillMissingValue(_))
    ));

    // vault with a malformed ref
    for bad in ["novault", "/key", "vault/", "a/b/c"] {
        let c = cfg(
            "https://x.example",
            vec![op(
                "a",
                vec![param(
                    "p",
                    Fill {
                        mode: FillMode::Vault,
                        vault_ref: Some(bad.into()),
                        ..Default::default()
                    },
                )],
            )],
        );
        assert!(
            matches!(validate_config(&c), Err(ConfigError::BadVaultRef(_))),
            "{bad:?} must be rejected as a vault_ref"
        );
    }

    let c = cfg(
        "https://x.example",
        vec![op(
            "a",
            vec![param(
                "p",
                Fill {
                    mode: FillMode::Vault,
                    vault_ref: Some("v/K".into()),
                    ..Default::default()
                },
            )],
        )],
    );
    assert!(validate_config(&c).is_ok());
}

#[test]
fn operation_ids_are_unique_and_safe_for_mcp_names() {
    let dup = cfg("https://x.example", vec![op("a", vec![]), op("a", vec![])]);
    assert!(matches!(
        validate_config(&dup),
        Err(ConfigError::DuplicateOperation(_))
    ));

    for bad in ["", "has space", "has/slash", "-lead", "a.b", "a__b__c\n"] {
        let c = cfg("https://x.example", vec![op(bad, vec![])]);
        assert!(
            validate_config(&c).is_err(),
            "{bad:?} must be rejected as an operation id"
        );
    }
    for good in ["listThings", "get_user", "a-b", "_x", "v1"] {
        let c = cfg("https://x.example", vec![op(good, vec![])]);
        assert!(validate_config(&c).is_ok(), "{good:?} should be a legal id");
    }
}

/// §3d rule 4. This is the pre-filter only — the engine must re-check after DNS
/// resolution and after every redirect.
#[test]
fn ssrf_prefilter_denies_internal_targets() {
    for denied in [
        "localhost",
        "LOCALHOST",
        "foo.localhost",
        "wheel-host.railway.internal",
        "anything.internal",
        "printer.local",
        "127.0.0.1",
        "127.9.9.9",
        "0.0.0.0",
        "10.0.0.1",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "192.0.0.1",
        "198.18.0.1",
        "255.255.255.255",
        "::1",
        "fd00::1",
        "fe80::1",
        "::ffff:127.0.0.1",
        "::ffff:10.0.0.1",
    ] {
        assert!(host_is_denied(denied), "{denied:?} must be denied");
    }
    for allowed in [
        "api.example.com",
        "1.1.1.1",
        "8.8.8.8",
        "172.32.0.1",
        "172.15.0.1",
        "2606:4700::1111",
    ] {
        assert!(!host_is_denied(allowed), "{allowed:?} must be allowed");
    }
}

#[test]
fn base_url_must_be_absolute_http_and_publicly_routable() {
    for bad in [
        "api.example.com",
        "ftp://api.example.com",
        "https://",
        "http://localhost:8080",
        "https://127.0.0.1",
        "https://wheel-host.railway.internal/v1",
        "https://[::1]:443/v1",
    ] {
        let c = cfg(bad, vec![]);
        assert!(validate_config(&c).is_err(), "{bad:?} must be rejected");
    }
    for good in [
        "https://api.example.com",
        "http://api.example.com:8080/base",
        "https://user@api.example.com/v1",
        "https://[2606:4700::1111]/v1",
    ] {
        let c = cfg(good, vec![]);
        assert!(
            validate_config(&c).is_ok(),
            "{good:?} must be allowed: {:?}",
            validate_config(&c)
        );
    }
}
