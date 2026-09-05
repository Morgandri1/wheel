//! Every rejection `validate_config` makes.
//!
//! These are not shape checks — the JSON schema already refuses a malformed
//! config, and anything the schema catches is uninteresting here. What is left
//! is the set of configs that are perfectly well-formed JSON and still must not
//! exist: a path that escapes its prefix, a vault key that could smuggle a
//! second environment assignment, a tool pointed at the host's own network, a
//! column that would collide with the implicit primary key. Each one is a
//! decision the type system cannot make, so each one needs a test.

use wheel_core::{
    validate::{
        normalize_chest_key, url_host, validate_config, validate_endpoint_path, ConfigError,
        MAX_ENDPOINT_PATH, MAX_SCRIPT_TIMEOUT_SECS, MAX_SYSTEM_PROMPT, MAX_TABLE_COLUMNS,
    },
    AgentConfig, ChestConfig, Column, ColumnType, CtxConfig, EndpointConfig, HttpMethod, McpConfig,
    NodeConfig, ResponseMode, ScriptConfig, ScriptLanguage, TableConfig, VaultConfig,
};

fn col(name: &str) -> Column {
    Column {
        name: wheel_core::Ident::new(name).expect("valid ident"),
        column_type: ColumnType::Text,
    }
}

fn table(cols: &[&str]) -> NodeConfig {
    NodeConfig::Table(TableConfig {
        columns: cols.iter().map(|c| col(c)).collect(),
    })
}

fn endpoint(path: &str) -> NodeConfig {
    NodeConfig::Endpoint(EndpointConfig {
        method: HttpMethod::Post,
        path: path.to_string(),
        response_mode: ResponseMode::Ack,
        auth: Default::default(),
    })
}

fn script(source: &str, timeout: Option<u32>) -> NodeConfig {
    NodeConfig::Script(ScriptConfig {
        language: ScriptLanguage::Python,
        source: source.to_string(),
        timeout_secs: timeout,
    })
}

fn vault(keys: &[&str]) -> NodeConfig {
    NodeConfig::Vault(VaultConfig {
        keys: keys.iter().map(|k| k.to_string()).collect(),
    })
}

// --- endpoint paths --------------------------------------------------------
//
// These become public URLs at /p/<project>/<path>, so a path that escapes its
// prefix escapes the project.

#[test]
fn an_endpoint_path_must_be_absolute() {
    assert_eq!(
        validate_endpoint_path("hooks/github"),
        Err(ConfigError::PathNotAbsolute)
    );
    assert!(validate_endpoint_path("/hooks/github").is_ok());
}

#[test]
fn traversal_is_rejected_segment_wise_not_by_substring() {
    for bad in ["/..", "/../etc", "/a/../b", "/a/b/..", "/../"] {
        assert_eq!(
            validate_endpoint_path(bad),
            Err(ConfigError::PathTraversal),
            "{bad:?} escapes its prefix and must be refused"
        );
    }
    // ...but `..` inside a segment is just characters, and refusing it would
    // reject legitimate paths for looking alarming.
    for ok in ["/a..b", "/..b", "/a..", "/v1/file..txt"] {
        assert!(
            validate_endpoint_path(ok).is_ok(),
            "{ok:?} does not traverse and must be allowed"
        );
    }
}

#[test]
fn a_path_may_not_carry_a_query_or_fragment() {
    // Routing matches on the path alone; a query here would either be ignored
    // silently or make two endpoints indistinguishable.
    assert_eq!(
        validate_endpoint_path("/hook?token=x"),
        Err(ConfigError::PathHasQuery)
    );
    assert_eq!(
        validate_endpoint_path("/hook#frag"),
        Err(ConfigError::PathHasQuery)
    );
}

#[test]
fn an_over_long_path_is_refused_at_the_boundary() {
    let ok = format!("/{}", "a".repeat(MAX_ENDPOINT_PATH - 1));
    assert_eq!(ok.len(), MAX_ENDPOINT_PATH);
    assert!(
        validate_endpoint_path(&ok).is_ok(),
        "the limit is inclusive"
    );

    let too_long = format!("/{}", "a".repeat(MAX_ENDPOINT_PATH));
    assert_eq!(
        validate_endpoint_path(&too_long),
        Err(ConfigError::PathTooLong {
            max: MAX_ENDPOINT_PATH
        })
    );
}

#[test]
fn endpoint_config_validation_goes_through_the_path_rules() {
    assert_eq!(
        validate_config(&endpoint("relative")),
        Err(ConfigError::PathNotAbsolute)
    );
    assert!(validate_config(&endpoint("/ok")).is_ok());
}

// --- tables ----------------------------------------------------------------

#[test]
fn a_table_needs_at_least_one_column() {
    assert_eq!(validate_config(&table(&[])), Err(ConfigError::NoColumns));
}

#[test]
fn the_implicit_primary_key_column_may_not_be_redefined() {
    // Every table has an implicit `key TEXT PRIMARY KEY` that
    // `wheel write <table>/<row>` upserts on; a user column of the same name
    // would collide in the generated DDL.
    assert_eq!(
        validate_config(&table(&["key"])),
        Err(ConfigError::ReservedColumn("key".into()))
    );
}

#[test]
fn duplicate_columns_are_refused() {
    assert_eq!(
        validate_config(&table(&["a", "b", "a"])),
        Err(ConfigError::DuplicateColumn("a".into()))
    );
}

#[test]
fn the_column_ceiling_is_enforced_at_the_boundary() {
    let names: Vec<String> = (0..MAX_TABLE_COLUMNS).map(|i| format!("c{i}")).collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    assert!(
        validate_config(&table(&refs)).is_ok(),
        "the limit is inclusive"
    );

    let names: Vec<String> = (0..=MAX_TABLE_COLUMNS).map(|i| format!("c{i}")).collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    assert_eq!(
        validate_config(&table(&refs)),
        Err(ConfigError::TooManyColumns {
            max: MAX_TABLE_COLUMNS
        })
    );
}

// --- scripts ---------------------------------------------------------------

#[test]
fn an_empty_script_is_refused_including_one_that_is_only_whitespace() {
    for empty in ["", "   ", "\n\t\n"] {
        assert_eq!(
            validate_config(&script(empty, None)),
            Err(ConfigError::EmptyScript),
            "{empty:?} is not a script"
        );
    }
}

#[test]
fn a_script_timeout_must_be_inside_the_documented_range() {
    assert_eq!(
        validate_config(&script("print(1)", Some(0))),
        Err(ConfigError::BadTimeout {
            max: MAX_SCRIPT_TIMEOUT_SECS
        }),
        "a zero timeout would mean the script can never finish in time"
    );
    assert_eq!(
        validate_config(&script("print(1)", Some(MAX_SCRIPT_TIMEOUT_SECS + 1))),
        Err(ConfigError::BadTimeout {
            max: MAX_SCRIPT_TIMEOUT_SECS
        })
    );
    assert!(validate_config(&script("print(1)", Some(1))).is_ok());
    assert!(validate_config(&script("print(1)", Some(MAX_SCRIPT_TIMEOUT_SECS))).is_ok());
    // Absent means the default, which must itself be legal.
    assert!(validate_config(&script("print(1)", None)).is_ok());
}

// --- mcp -------------------------------------------------------------------

#[test]
fn an_mcp_stdio_server_needs_a_command() {
    let cfg = NodeConfig::Mcp(McpConfig::Stdio {
        command: "   ".into(),
        args: None,
        env: None,
    });
    assert_eq!(validate_config(&cfg), Err(ConfigError::McpMissingCommand));
}

#[test]
fn an_mcp_http_server_needs_a_real_url() {
    let http = |url: &str| {
        NodeConfig::Mcp(McpConfig::Http {
            url: url.to_string(),
            env: None,
        })
    };
    assert_eq!(
        validate_config(&http("  ")),
        Err(ConfigError::McpMissingUrl)
    );
    assert_eq!(
        validate_config(&http("ftp://example.com")),
        Err(ConfigError::McpBadUrl)
    );
    assert!(validate_config(&http("https://example.com/mcp")).is_ok());
}

/// An MCP url is an outbound target the engine will connect to, exactly like a
/// tool's base_url, so it gets the same SSRF pre-filter. Without this an `mcp`
/// node is a hole straight through the tool-node policy.
#[test]
fn an_mcp_url_may_not_point_at_the_hosts_own_network() {
    for denied in [
        "http://127.0.0.1:7000/mcp",
        "http://localhost/mcp",
        "http://10.0.0.5/mcp",
        "http://192.168.1.1/mcp",
        "http://169.254.169.254/latest/meta-data",
        "http://[::1]/mcp",
        "https://something.railway.internal/mcp",
    ] {
        let cfg = NodeConfig::Mcp(McpConfig::Http {
            url: denied.to_string(),
            env: None,
        });
        assert_eq!(
            validate_config(&cfg),
            Err(ConfigError::McpBadUrl),
            "{denied} must not be reachable from a tenant"
        );
    }
}

// --- vault keys ------------------------------------------------------------

/// Vault keys are exported as environment variables into agent children. A key
/// that is not a legal env var name is either silently dropped or a way to
/// smuggle a second assignment, so the charset is closed rather than filtered.
#[test]
fn a_vault_key_must_be_a_legal_environment_variable_name() {
    for bad in [
        "",
        "1LEADING_DIGIT",
        "lower_case",
        "HAS-DASH",
        "HAS SPACE",
        "HAS=EQUALS",
        "HAS\nNEWLINE",
        "PATH=/evil:x",
        "HAS.DOT",
    ] {
        assert_eq!(
            validate_config(&vault(&[bad])),
            Err(ConfigError::BadVaultKey(bad.to_string())),
            "{bad:?} is not a usable env var name"
        );
    }
    for ok in ["A", "API_KEY", "X2", "_LEADING_UNDERSCORE", "A_1_B"] {
        assert!(
            validate_config(&vault(&[ok])).is_ok(),
            "{ok:?} is a legal env var name"
        );
    }
}

#[test]
fn duplicate_vault_keys_are_refused() {
    assert_eq!(
        validate_config(&vault(&["A", "B", "A"])),
        Err(ConfigError::DuplicateVaultKey("A".into()))
    );
}

// --- agents ----------------------------------------------------------------

#[test]
fn a_system_prompt_has_a_ceiling_and_the_boundary_is_allowed() {
    let mut cfg = AgentConfig {
        system_prompt: "x".repeat(MAX_SYSTEM_PROMPT),
        ..Default::default()
    };
    assert!(validate_config(&NodeConfig::Agent(cfg.clone())).is_ok());

    cfg.system_prompt = "x".repeat(MAX_SYSTEM_PROMPT + 1);
    assert_eq!(
        validate_config(&NodeConfig::Agent(cfg)),
        Err(ConfigError::SystemPromptTooLong {
            max: MAX_SYSTEM_PROMPT
        })
    );
}

#[test]
fn configs_with_nothing_to_validate_are_accepted_rather_than_forgotten() {
    // ctx and chest have no constraints today. Asserting that explicitly means
    // adding one later breaks a test rather than passing silently.
    assert!(validate_config(&NodeConfig::Ctx(CtxConfig {
        markdown: String::new()
    }))
    .is_ok());
    assert!(validate_config(&NodeConfig::Chest(ChestConfig::default())).is_ok());
}

// --- url_host --------------------------------------------------------------
//
// The SSRF filter is only as good as the host it is given, so the parser that
// extracts it is security-relevant on its own.

#[test]
fn the_host_is_extracted_without_scheme_userinfo_or_port() {
    for (url, want) in [
        ("https://example.com/x", "example.com"),
        ("http://example.com:8080/x", "example.com"),
        ("https://user:pass@example.com/x", "example.com"),
        // userinfo is the classic disguise: everything before the LAST `@`.
        ("https://evil.com@127.0.0.1/x", "127.0.0.1"),
        ("https://a@b@127.0.0.1:80/x", "127.0.0.1"),
        ("https://[::1]:7000/x", "::1"),
        ("https://example.com", "example.com"),
        ("https://example.com?q=1", "example.com"),
        ("https://example.com#f", "example.com"),
    ] {
        assert_eq!(
            url_host(url).as_deref(),
            Some(want),
            "{url} should resolve to host {want}"
        );
    }
}

#[test]
fn a_url_with_no_usable_host_yields_none() {
    for bad in ["ftp://example.com", "example.com", "https://", "http://"] {
        assert_eq!(url_host(bad), None, "{bad:?} has no http(s) host");
    }
}

/// The disguise that matters: a URL whose visible host looks public but whose
/// real host is loopback must still be denied.
#[test]
fn userinfo_cannot_disguise_a_denied_host() {
    let cfg = NodeConfig::Mcp(McpConfig::Http {
        url: "https://totally-public.example.com@127.0.0.1:7000/mcp".into(),
        env: None,
    });
    assert_eq!(validate_config(&cfg), Err(ConfigError::McpBadUrl));
}

// --- chest keys ------------------------------------------------------------

#[test]
fn chest_keys_are_normalised_rather_than_merely_checked() {
    assert_eq!(normalize_chest_key("a/b.txt").unwrap(), "a/b.txt");
    assert_eq!(normalize_chest_key("./a//b.txt").unwrap(), "a/b.txt");
    assert_eq!(normalize_chest_key("a/./b").unwrap(), "a/b");
    assert_eq!(normalize_chest_key("a///b").unwrap(), "a/b");
}

#[test]
fn a_chest_key_may_not_escape_its_directory() {
    for bad in [
        "../secrets",
        "a/../../etc/passwd",
        "/etc/passwd",
        "..",
        "a/..",
        "",
        "/",
        "./",
        "a\\b",
        "a\0b",
    ] {
        assert_eq!(
            normalize_chest_key(bad),
            Err(ConfigError::PathTraversal),
            "{bad:?} must not resolve to a blob path"
        );
    }
}

/// Normalisation must not be a way to smuggle traversal past the check: the
/// result of normalising is what hits the filesystem, so it is what must be
/// safe.
#[test]
fn a_normalised_key_never_contains_a_traversal_segment() {
    for key in ["a/./b", "./x", "a//b/./c", "x/y/z"] {
        let out = normalize_chest_key(key).unwrap();
        assert!(
            !out.starts_with('/'),
            "{key:?} normalised to absolute {out:?}"
        );
        assert!(
            !out.split('/')
                .any(|s| s == ".." || s == "." || s.is_empty()),
            "{key:?} normalised to {out:?}, which still has a dot segment"
        );
    }
}
