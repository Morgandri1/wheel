//! The canonical JSON in ARCHITECTURE.md §3 is a cross-team contract: Web
//! generates TypeScript from it and the API forwards it verbatim. These tests
//! pin the exact wire shape for all 8 node types.

use serde_json::json;
use uuid::Uuid;
use wheel_core::*;

fn uid(n: u8) -> Uuid {
    Uuid::from_bytes([n; 16])
}

#[test]
fn agent_node_matches_the_contract_example() {
    let node = Node {
        id: uid(1),
        name: NodeName::new("researcher").unwrap(),
        position: Position::new(120.0, 340.0),
        wires: vec![Wire::new(uid(2), WireType::Read)],
        config: NodeConfig::Agent(AgentConfig {
            harness: Harness::Claude,
            model: None,
            system_prompt: "be brief".into(),
            run_on_startup: false,
            ephemeral_context: false,
            ..Default::default()
        }),
    };
    let v = serde_json::to_value(&node).unwrap();
    assert_eq!(
        v,
        json!({
            "id": "01010101-0101-0101-0101-010101010101",
            "name": "researcher",
            "position": { "x": 120.0, "y": 340.0 },
            "wires": [ { "to": "02020202-0202-0202-0202-020202020202", "type": "read" } ],
            "type": "agent",
            "config": {
                "harness": "claude",
                "system_prompt": "be brief",
                "run_on_startup": false,
                "ephemeral_context": false
            }
        })
    );
    // Round-trips.
    let back: Node = serde_json::from_value(v).unwrap();
    assert_eq!(back, node);
    assert_eq!(back.node_type(), NodeType::Agent);
}

/// Build one node of every type and assert the `type` tag and `config` payload
/// land as the flat `"type" + "config"` pair the contract specifies.
#[test]
fn every_node_type_round_trips_with_correct_tag() {
    let cases: Vec<(NodeType, NodeConfig)> = vec![
        (
            NodeType::Agent,
            NodeConfig::Agent(AgentConfig {
                harness: Harness::Codex,
                model: Some("gpt-5".into()),
                system_prompt: "hi".into(),
                run_on_startup: true,
                ephemeral_context: true,
                ..Default::default()
            }),
        ),
        (
            NodeType::Ctx,
            NodeConfig::Ctx(CtxConfig {
                markdown: "# notes".into(),
            }),
        ),
        (
            NodeType::Table,
            NodeConfig::Table(TableConfig {
                columns: vec![Column {
                    name: Ident::new("title").unwrap(),
                    column_type: ColumnType::Text,
                }],
            }),
        ),
        (
            NodeType::Endpoint,
            NodeConfig::Endpoint(EndpointConfig {
                method: HttpMethod::Post,
                path: "/hook".into(),
                response_mode: ResponseMode::Ack,
                auth: EndpointAuth::None,
            }),
        ),
        (
            NodeType::Script,
            NodeConfig::Script(ScriptConfig {
                language: ScriptLanguage::Python,
                source: "print(1)".into(),
                timeout_secs: None,
            }),
        ),
        (
            NodeType::Mcp,
            NodeConfig::Mcp(McpConfig::Stdio {
                command: "npx".into(),
                args: Some(vec!["-y".into()]),
                env: None,
            }),
        ),
        (
            NodeType::Vault,
            NodeConfig::Vault(VaultConfig {
                keys: vec!["OPENAI_API_KEY".into()],
            }),
        ),
        (NodeType::Chest, NodeConfig::Chest(ChestConfig {})),
        (
            NodeType::Tool,
            NodeConfig::Tool(ToolConfig {
                kind: ToolKind::Http,
                source: ToolSource {
                    format: ToolFormat::Openapi,
                    raw: "{}".into(),
                    imported_at: Timestamp::parse_rfc3339("2026-09-05T00:00:00Z").unwrap(),
                },
                base_url: "https://api.example.com".into(),
                operations: vec![],
            }),
        ),
    ];

    // Tied to NodeType::ALL, not a literal: adding a node type must break this
    // test rather than silently skipping coverage of the new one.
    assert_eq!(
        cases.len(),
        NodeType::ALL.len(),
        "every node type needs a case here"
    );
    for t in NodeType::ALL {
        assert!(
            cases.iter().any(|(ty, _)| *ty == t),
            "no case covers node type {t}"
        );
    }

    for (expected_type, config) in cases {
        assert_eq!(config.node_type(), expected_type);
        let node = Node::new(
            uid(9),
            NodeName::new("n1").unwrap(),
            Position::default(),
            config,
        );
        let v = serde_json::to_value(&node).unwrap();
        assert_eq!(
            v["type"],
            json!(expected_type.as_str()),
            "tag for {expected_type}"
        );
        assert!(
            v.get("config").is_some(),
            "config present for {expected_type}"
        );
        // The tag must NOT also leak inside config.
        assert!(
            v["config"].get("type").is_none() || expected_type == NodeType::Endpoint,
            "type tag leaked into config for {expected_type}"
        );
        let back: Node = serde_json::from_value(v).unwrap();
        assert_eq!(back, node, "round-trip for {expected_type}");
    }
}

#[test]
fn optional_agent_fields_are_omitted_when_none() {
    let cfg = NodeConfig::Agent(AgentConfig {
        harness: Harness::Claude,
        model: None,
        system_prompt: String::new(),
        run_on_startup: false,
        ephemeral_context: false,
        ..Default::default()
    });
    let v = serde_json::to_value(&cfg).unwrap();
    assert!(
        v["config"].get("model").is_none(),
        "null model must be omitted, not null"
    );
}

#[test]
fn a_config_of_the_wrong_type_cannot_be_deserialized() {
    // type says ctx, config is an agent config -> must fail, not silently pick one.
    let bad = json!({
        "id": "01010101-0101-0101-0101-010101010101",
        "name": "x", "position": {"x":0.0,"y":0.0}, "wires": [],
        "type": "ctx",
        "config": { "harness": "claude", "system_prompt": "", "run_on_startup": false, "ephemeral_context": false }
    });
    assert!(serde_json::from_value::<Node>(bad).is_err());
}

#[test]
fn invalid_names_are_rejected_at_deserialization() {
    for bad in [
        "",              // empty
        "-leading",      // must start alnum
        "_leading",      // must start alnum
        "Upper",         // no uppercase
        "has space",     // no spaces
        "has.dot",       // charset
        "has/slash",     // charset — critical: names appear in `wheel read a/b`
        "user",          // reserved: it is the UI sender label
        "wheel",         // reserved
        &"a".repeat(64), // too long (max 63)
    ] {
        assert!(
            NodeName::new(bad).is_err(),
            "{bad:?} should be rejected as a node name"
        );
        let v = json!({
            "id": "01010101-0101-0101-0101-010101010101",
            "name": bad, "position": {"x":0.0,"y":0.0}, "wires": [],
            "type": "chest", "config": {}
        });
        assert!(
            serde_json::from_value::<Node>(v).is_err(),
            "{bad:?} should be rejected when deserializing a Node"
        );
    }
    // ...and the boundary cases that must be ACCEPTED.
    for good in ["a", "0", "a-b_c", &"a".repeat(63), "9lives", "x_"] {
        assert!(
            NodeName::new(good).is_ok(),
            "{good:?} should be a legal name"
        );
    }
}

#[test]
fn table_columns_may_be_called_user_but_not_key() {
    // `user` is reserved for NODES (message sender label), not columns.
    assert!(Ident::new("user").is_ok());
    // `key` is the implicit primary key column.
    let cfg = NodeConfig::Table(TableConfig {
        columns: vec![Column {
            name: Ident::new(TABLE_KEY_COLUMN).unwrap(),
            column_type: ColumnType::Text,
        }],
    });
    assert!(matches!(
        validate_config(&cfg),
        Err(ConfigError::ReservedColumn(_))
    ));
    // A column may not contain '-' (it is a bare sqlite identifier).
    assert!(Ident::new("a-b").is_err());
}

#[test]
fn events_use_the_documented_dotted_type_tags() {
    let e = Event::BoardChanged {
        at: Timestamp::parse_rfc3339("2026-09-05T00:21:00Z").unwrap(),
    };
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["type"], json!("board.changed"));
    assert_eq!(v["at"], json!("2026-09-05T00:21:00Z"));

    let e = Event::NodeState {
        node_id: uid(3),
        state: NodeState::Agent(AgentState {
            status: AgentStatus::NeedsAuth,
            ..Default::default()
        }),
    };
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["type"], json!("node.state"));
    assert_eq!(v["state"]["status"], json!("needs_auth"));
}

#[test]
fn vault_config_carries_key_names_only_and_validates_them_as_env_names() {
    let ok = NodeConfig::Vault(VaultConfig {
        keys: vec!["API_KEY".into(), "A1_B2".into()],
    });
    assert!(validate_config(&ok).is_ok());
    for bad in ["lower", "1LEADING", "HAS-DASH", "HAS SPACE", ""] {
        let cfg = NodeConfig::Vault(VaultConfig {
            keys: vec![bad.into()],
        });
        assert!(
            validate_config(&cfg).is_err(),
            "{bad:?} must be rejected as a vault key"
        );
    }
}

#[test]
fn endpoint_paths_reject_traversal_and_query_strings() {
    for bad in ["no-leading-slash", "/a/../b", "/..", "/a?b=1", "/a#f"] {
        assert!(
            validate_endpoint_path(bad).is_err(),
            "{bad:?} must be rejected"
        );
    }
    for good in ["/", "/hook", "/a/b/c", "/a..b"] {
        assert!(
            validate_endpoint_path(good).is_ok(),
            "{good:?} must be allowed"
        );
    }
}

#[test]
fn chest_keys_are_normalized_and_traversal_is_refused() {
    assert_eq!(normalize_chest_key("a//b/./c").unwrap(), "a/b/c");
    for bad in ["../x", "a/../../b", "/abs", "a\\b", "", ".", "//"] {
        assert!(
            normalize_chest_key(bad).is_err(),
            "{bad:?} must be refused as a chest key"
        );
    }
}

#[test]
fn listen_addr_parses_both_sandbox_backends() {
    use std::path::PathBuf;
    use wheel_core::ListenAddr;

    assert_eq!(
        ListenAddr::parse("tcp://0.0.0.0:7000").unwrap(),
        ListenAddr::Tcp("0.0.0.0:7000".into())
    );
    assert_eq!(
        ListenAddr::parse("unix:///run/wheel/abc/engine.sock").unwrap(),
        ListenAddr::Unix(PathBuf::from("/run/wheel/abc/engine.sock"))
    );
    assert_eq!(ListenAddr::default_tcp().to_string(), "tcp://0.0.0.0:7000");

    // Round-trips through Display.
    for raw in ["tcp://127.0.0.1:1", "unix:///a/b.sock"] {
        assert_eq!(ListenAddr::parse(raw).unwrap().to_string(), raw);
    }
    // Misconfiguration must fail loudly at boot, not bind somewhere useless.
    for bad in [
        "7000",
        "http://x:1",
        "tcp://",
        "tcp://hostonly",
        "tcp://host:99999",
        "unix://relative/path.sock",
    ] {
        assert!(ListenAddr::parse(bad).is_err(), "{bad:?} must be rejected");
    }
}

#[test]
fn board_reports_state_as_null_for_non_agent_nodes_not_omitted() {
    // §3: `GET /v1/board` returns `{ ...node, state }`, state null for
    // non-agents. Web branches on it, and an omitted key reads as
    // "not loaded yet" rather than "has no state".
    let nws = NodeWithState {
        node: Node::new(
            uid(7),
            NodeName::new("notes").unwrap(),
            Position::default(),
            NodeConfig::Ctx(CtxConfig {
                markdown: String::new(),
            }),
        ),
        state: None,
    };
    let v = serde_json::to_value(&nws).unwrap();
    assert!(v.get("state").is_some(), "state key must be present");
    assert_eq!(v["state"], json!(null));
    // Node fields are flattened alongside, not nested.
    assert_eq!(v["type"], json!("ctx"));
    assert_eq!(v["name"], json!("notes"));

    let agent = NodeWithState {
        node: Node::new(
            uid(8),
            NodeName::new("worker").unwrap(),
            Position::default(),
            NodeConfig::Agent(AgentConfig::default()),
        ),
        state: Some(NodeState::Agent(AgentState {
            status: AgentStatus::Parked,
            hosted_on: Some("cloud".into()),
            ..Default::default()
        })),
    };
    let v = serde_json::to_value(&agent).unwrap();
    assert_eq!(v["state"]["status"], json!("parked"));
    assert_eq!(v["state"]["hosted_on"], json!("cloud"));
}

#[test]
fn all_agent_statuses_serialize_as_the_contract_spells_them() {
    let expect = [
        (AgentStatus::Stopped, "stopped"),
        (AgentStatus::Starting, "starting"),
        (AgentStatus::NeedsAuth, "needs_auth"),
        (AgentStatus::Running, "running"),
        (AgentStatus::Idle, "idle"),
        (AgentStatus::Parked, "parked"),
        (AgentStatus::BudgetExhausted, "budget_exhausted"),
        (AgentStatus::Error, "error"),
    ];
    for (s, want) in expect {
        assert_eq!(serde_json::to_value(s).unwrap(), json!(want));
        assert_eq!(s.as_str(), want);
    }
    // A parked agent has no live process; is_live must not claim otherwise, or
    // the supervisor would try to write to a stdin that isn't there.
    assert!(!AgentStatus::Parked.is_live());
    assert!(!AgentStatus::BudgetExhausted.is_live());
    assert!(AgentStatus::Idle.is_live());
    assert!(AgentStatus::Running.is_live());
}

#[test]
fn unhosted_is_representable_and_distinct_from_cloud() {
    // §3e: `unhosted` is a first-class alarming state, not an absence.
    let s = AgentState::default();
    assert_eq!(s.hosted_on, None);
    let v = serde_json::to_value(&s).unwrap();
    assert!(v.get("hosted_on").is_some(), "hosted_on must be present");
    assert_eq!(v["hosted_on"], json!(null));
}

#[test]
fn idle_timeout_defaults_to_300_and_zero_disables_parking() {
    let c = AgentConfig::default();
    assert_eq!(c.idle_timeout_secs(), DEFAULT_IDLE_TIMEOUT_SECS);
    assert_eq!(DEFAULT_IDLE_TIMEOUT_SECS, 300);
    let c = AgentConfig {
        idle_timeout_secs: Some(0),
        ..Default::default()
    };
    assert_eq!(c.idle_timeout_secs(), 0);
}

/// Regression: the supervisor writes a `transcript` log stream (§3c#10), but
/// LogStream had no such variant, so every transcript line failed to parse into
/// an event and was silently DROPPED from the WebSocket — persisted to the
/// database, never broadcast. Web caught this; my own end-to-end test missed it
/// because it only asserted that *a* log event arrived.
#[test]
fn every_stream_the_engine_writes_round_trips_as_a_log_stream() {
    for name in ["stdout", "stderr", "engine", "transcript"] {
        let parsed: LogStream =
            serde_json::from_value(json!(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(serde_json::to_value(parsed).unwrap(), json!(name));
    }
    // ...and an unknown stream is still rejected, so the filter cannot silently
    // stop filtering.
    assert!(serde_json::from_value::<LogStream>(json!("bogus")).is_err());
}

/// The `lagged` frame travels on the events socket, so a client typing the
/// union from the schema must find it there. Otherwise it lands in the default
/// branch, and the natural default — tear down and reconnect — is exactly wrong
/// at the moment the socket is healthy and merely behind.
#[test]
fn the_lagged_frame_is_part_of_the_event_union() {
    let e = Event::Lagged {
        hint: LAGGED_HINT.to_string(),
    };
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["type"], json!("lagged"));
    assert_eq!(
        v["hint"],
        json!("events were dropped; refetch GET /v1/board")
    );
    // Round-trips, so a client can deserialize it with the same union it uses
    // for every other frame.
    let back: Event = serde_json::from_value(v).unwrap();
    assert_eq!(back, e);
}

/// Denials are read by people, so they should read as English. "a agent node"
/// was a small but real papercut in the most-seen error message we have.
#[test]
fn denial_messages_use_the_right_article() {
    let err = check_wire(
        uid(1),
        NodeType::Agent,
        uid(2),
        NodeType::Vault,
        WireType::Write,
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "an agent node may not have a write wire to a vault node"
    );

    // Both vowel-initial types get "an", every other type gets "a".
    for t in NodeType::ALL {
        let expected = match t {
            NodeType::Agent | NodeType::Endpoint => "an",
            _ => "a",
        };
        assert_eq!(t.article(), expected, "article for {t}");
    }
}
