// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! What a caller below `admin` may learn from a node's config.
//!
//! `GET /v1/board` is a guest route, so the board JSON is the one place a project's whole credential
//! map is handed out in a single response. These tests are written as a **canary sweep**: every
//! credential-bearing field is filled with a string nothing else in the fixture contains, the
//! redacted config is serialized, and the serialized text must not contain any of them.
//!
//! Written that way on purpose. Asserting `config.keys == []` field by field tests the fields
//! somebody remembered; searching the serialized output for a canary tests the *output*, which is
//! what actually reaches a guest — and it catches a credential that reaches the wire through a
//! field nobody thought to assert on.

use std::collections::BTreeMap;
use wheel_core::node::RedactCredentials;
use wheel_core::*;

/// Strings that must never survive redaction. Distinct from each other and from anything
/// structural, so a failure names which one leaked.
const VAULT_KEY: &str = "CANARY-VAULT-KEY-NAME";
const STATIC_SECRET: &str = "CANARY-Bearer-sk_live_static_secret";
const TOOL_REF: &str = "CANARY-creds/TOOL_VAULT_REF";
const ENDPOINT_REF: &str = "CANARY-creds/ENDPOINT_BEARER_REF";
const GIT_REF: &str = "CANARY-creds/GIT_TOKEN_REF";
const MCP_ENV_KEY: &str = "CANARY_MCP_ENV_NAME";
const MCP_ENV_VALUE: &str = "CANARY-mcp-env-value";

const ALL_CANARIES: &[&str] = &[
    VAULT_KEY,
    STATIC_SECRET,
    TOOL_REF,
    ENDPOINT_REF,
    GIT_REF,
    MCP_ENV_KEY,
    MCP_ENV_VALUE,
];

fn vault() -> NodeConfig {
    NodeConfig::Vault(VaultConfig {
        keys: vec![VAULT_KEY.into(), "SECOND_KEY_NAME".into()],
    })
}

fn tool() -> NodeConfig {
    let param = |name: &str, fill: Fill| ToolParam {
        name: name.into(),
        location: ParamLocation::Header,
        required: true,
        description: None,
        schema: None,
        fill,
    };
    NodeConfig::Tool(ToolConfig {
        kind: ToolKind::Http,
        source: ToolSource {
            format: ToolFormat::Manual,
            raw: String::new(),
            imported_at: Timestamp::parse_rfc3339("2026-09-12T00:00:00Z").unwrap(),
        },
        base_url: "https://api.example.test".into(),
        operations: vec![ToolOperation {
            id: "charge".into(),
            method: ToolMethod::Post,
            path: "/v1/charges".into(),
            summary: None,
            enabled: true,
            params: vec![
                param(
                    "authorization",
                    Fill {
                        mode: FillMode::Static,
                        value: Some(STATIC_SECRET.into()),
                        vault_ref: None,
                    },
                ),
                param(
                    "x-key",
                    Fill {
                        mode: FillMode::Vault,
                        value: None,
                        vault_ref: Some(TOOL_REF.into()),
                    },
                ),
                param("amount", Fill::agent()),
            ],
        }],
    })
}

fn endpoint() -> NodeConfig {
    NodeConfig::Endpoint(EndpointConfig {
        method: HttpMethod::Post,
        path: "/hook".into(),
        response_mode: ResponseMode::Ack,
        auth: EndpointAuth::Bearer {
            vault_ref: ENDPOINT_REF.into(),
        },
    })
}

fn agent_with_git() -> NodeConfig {
    NodeConfig::Agent(AgentConfig {
        system_prompt: "you are helpful".into(),
        workspaces: vec![Workspace {
            path: "repo".into(),
            git: Some(GitSource {
                url: "https://github.com/example/repo.git".into(),
                git_ref: Some("main".into()),
                vault_ref: Some(GIT_REF.into()),
            }),
        }],
        ..Default::default()
    })
}

fn mcp() -> NodeConfig {
    let mut env = BTreeMap::new();
    env.insert(MCP_ENV_KEY.to_string(), MCP_ENV_VALUE.to_string());
    NodeConfig::Mcp(McpConfig::Stdio {
        command: "mcp-server".into(),
        args: Some(vec!["--port".into(), "1".into()]),
        env: Some(env),
    })
}

/// Every config that carries a credential, so a `for` loop covers the whole surface rather than
/// whichever one the author was thinking about.
fn credential_bearing() -> Vec<(&'static str, NodeConfig)> {
    vec![
        ("vault", vault()),
        ("tool", tool()),
        ("endpoint", endpoint()),
        ("agent workspace", agent_with_git()),
        ("mcp", mcp()),
    ]
}

fn json(config: &NodeConfig) -> String {
    serde_json::to_string(config).expect("a config serializes")
}

/// **The gate.** Nothing a guest receives may contain a canary.
#[test]
fn no_credential_survives_redaction() {
    for (label, config) in credential_bearing() {
        let before = json(&config);
        let after = json(&config.redact_credentials());

        for canary in ALL_CANARIES {
            if before.contains(canary) {
                assert!(
                    !after.contains(canary),
                    "{label}: {canary} survived redaction\n  before: {before}\n  after:  {after}"
                );
            }
        }
    }
}

/// The fixture has to actually contain what it claims to, or the test above passes by testing
/// nothing — the failure mode §0b calls "a test that cannot fail".
#[test]
fn the_fixture_really_carries_every_canary_before_redaction() {
    let all: String = credential_bearing()
        .iter()
        .map(|(_, c)| json(c))
        .collect::<Vec<_>>()
        .join("\n");
    for canary in ALL_CANARIES {
        assert!(
            all.contains(canary),
            "{canary} is not in the unredacted fixture, so redacting it proves nothing"
        );
    }
}

/// Redaction must not be so broad that a shared board is unreadable. A guest is supposed to see the
/// board — just not which secret each part reaches for.
#[test]
fn structure_survives_redaction() {
    let redacted = json(&tool().redact_credentials());
    assert!(redacted.contains("https://api.example.test"), "{redacted}");
    assert!(redacted.contains("charge"), "{redacted}");
    assert!(redacted.contains("amount"), "{redacted}");
    // The MODE stays, so a reader still learns the field is vault-filled — just not from where.
    assert!(
        redacted.contains("vault"),
        "the fill mode was removed too: {redacted}"
    );

    let redacted = json(&agent_with_git().redact_credentials());
    assert!(
        redacted.contains("github.com/example/repo.git"),
        "{redacted}"
    );
    assert!(redacted.contains("main"), "{redacted}");

    let redacted = json(&mcp().redact_credentials());
    assert!(redacted.contains("mcp-server"), "{redacted}");

    let redacted = json(&endpoint().redact_credentials());
    assert!(redacted.contains("/hook"), "{redacted}");
}

/// A protected endpoint must not be described as a public one. Redacting `Bearer` down to `None`
/// would be misinformation of the worst kind: it reads as "anyone may call this".
#[test]
fn a_bearer_endpoint_still_says_it_requires_a_bearer() {
    let redacted = endpoint().redact_credentials();
    match &redacted {
        NodeConfig::Endpoint(c) => match &c.auth {
            EndpointAuth::Bearer { vault_ref } => {
                assert!(vault_ref.is_empty(), "the ref survived: {vault_ref}")
            }
            EndpointAuth::None => panic!("a protected endpoint was redacted into a public one"),
        },
        other => panic!("wrong variant: {other:?}"),
    }
}

/// A config with nothing to hide comes back byte-identical, so redaction cannot quietly damage the
/// parts of the board sharing exists to show.
#[test]
fn a_config_with_no_credentials_is_unchanged() {
    for config in [
        NodeConfig::Ctx(CtxConfig {
            markdown: "# notes".into(),
        }),
        NodeConfig::Chest(ChestConfig {}),
        NodeConfig::Script(ScriptConfig {
            language: ScriptLanguage::Python,
            source: "print(1)".into(),
            timeout_secs: Some(10),
        }),
    ] {
        assert_eq!(json(&config.redact_credentials()), json(&config));
        assert!(
            !config.has_redactable_credentials(),
            "reported as redactable: {config:?}"
        );
    }
}

/// `has_redactable_credentials` is what tells a client "this is hidden" rather than "there is
/// none", so it has to agree with what redaction actually changes.
#[test]
fn the_redaction_flag_agrees_with_the_redaction() {
    for (label, config) in credential_bearing() {
        assert!(
            config.has_redactable_credentials(),
            "{label} carries a credential but does not report it"
        );
        assert_ne!(
            json(&config.redact_credentials()),
            json(&config),
            "{label} reports a credential but redaction changed nothing"
        );
        // And once redacted there is nothing left to redact — redaction is idempotent, so a
        // double-projection cannot report a hidden field that is already gone.
        let once = config.redact_credentials();
        assert!(
            !once.has_redactable_credentials(),
            "{label} is still redactable"
        );
        assert_eq!(json(&once.redact_credentials()), json(&once), "{label}");
    }
}

/// An empty vault reports nothing hidden: a guest seeing "no keys" for a vault that genuinely has
/// none is the truth, and flagging it would train people to ignore the flag.
#[test]
fn an_empty_vault_has_nothing_to_hide() {
    let empty = NodeConfig::Vault(VaultConfig { keys: Vec::new() });
    assert!(!empty.has_redactable_credentials());
    assert_eq!(json(&empty.redact_credentials()), json(&empty));
}
