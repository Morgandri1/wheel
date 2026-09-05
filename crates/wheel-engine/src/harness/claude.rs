//! The Claude Code adapter. Verified against `claude` 2.1.261.

use std::ffi::OsString;

use super::{Harness, HarnessEvent, SpawnSpec, StartupFailure};

/// The Claude Code driver.
///
/// Named `ClaudeDriver` rather than `Claude` so it never reads as the product
/// name in call sites like `ClaudeDriver.parse_line(..)`.
pub struct ClaudeDriver;

/// Substring of the refusal `claude` prints when `bypassPermissions` is used as
/// root. That case exits 1 with EMPTY stdout — identical to being logged out —
/// so this string is the only thing that tells the two apart.
const ROOT_REFUSAL: &str = "cannot be used with root/sudo privileges";

impl Harness for ClaudeDriver {
    fn argv(&self, spec: &SpawnSpec) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec![
            "--print".into(),
            "--input-format".into(),
            "stream-json".into(),
            "--output-format".into(),
            "stream-json".into(),
            // Required by the CLI for stream-json output, not optional.
            "--verbose".into(),
            // A headless child would deadlock on a permission prompt. The
            // sandbox, not the agent's restraint, is the boundary.
            "--permission-mode".into(),
            "bypassPermissions".into(),
            // By PATH: argv is world-readable across uids and the composed
            // preamble contains injected ctx.
            "--append-system-prompt-file".into(),
            spec.prompt_file.clone().into_os_string(),
        ];
        if let Some(m) = &spec.model {
            a.push("--model".into());
            a.push(m.into());
        }
        if let Some(mcp) = &spec.mcp_config {
            a.push("--mcp-config".into());
            a.push(mcp.clone().into_os_string());
        }
        if let Some(session) = &spec.resume {
            a.push("--resume".into());
            a.push(session.into());
        }
        a
    }

    fn env(&self, spec: &SpawnSpec) -> Vec<(String, String)> {
        vec![
            // Isolates credentials AND .claude.json per node, which is what
            // lets two agents in one sandbox be two different accounts.
            (
                "CLAUDE_CONFIG_DIR".into(),
                spec.config_dir.display().to_string(),
            ),
            ("HOME".into(), spec.config_dir.display().to_string()),
            // Belt and braces with running non-root: without one of these,
            // bypassPermissions is refused outright.
            ("IS_SANDBOX".into(), "1".into()),
        ]
    }

    fn encode_turn(&self, envelope: &str) -> String {
        let turn = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": [ { "type": "text", "text": envelope } ] }
        });
        format!("{turn}\n")
    }

    fn parse_line(&self, line: &str) -> HarnessEvent {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return HarnessEvent::Unknown { raw: String::new() };
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            // Not JSON. Log it and carry on — never fatal.
            return HarnessEvent::Unknown {
                raw: line.to_string(),
            };
        };
        let session_id = v
            .get("session_id")
            .and_then(|s| s.as_str())
            .map(str::to_string);

        match v.get("type").and_then(|t| t.as_str()) {
            Some("system") if v.get("subtype").and_then(|s| s.as_str()) == Some("init") => {
                match session_id {
                    Some(session_id) => HarnessEvent::Init { session_id },
                    // An init without a session id is unusable for the
                    // session-matching F008 relies on, so it is not an init.
                    None => HarnessEvent::Unknown {
                        raw: line.to_string(),
                    },
                }
            }
            Some("assistant") => {
                let text = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                HarnessEvent::Text { session_id, text }
            }
            Some("result") => HarnessEvent::Result {
                session_id,
                is_error: v.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false),
                text: v.get("result").and_then(|r| r.as_str()).map(str::to_string),
            },
            // Everything else — including event types we have never seen — is
            // log material, not an error.
            _ => HarnessEvent::Unknown {
                raw: line.to_string(),
            },
        }
    }

    fn classify_startup_failure(&self, code: Option<i32>, stderr: &str) -> StartupFailure {
        if stderr.contains(ROOT_REFUSAL) {
            return StartupFailure::Misconfigured(
                "claude refuses --permission-mode bypassPermissions as root; \
                 run the child non-root or set IS_SANDBOX=1"
                    .into(),
            );
        }

        // Only a RECOGNISED auth message means needs_auth. Everything else —
        // including an unrecognised error and a bare exit 1 with no output — is
        // misconfiguration, because guessing needs_auth would send the operator
        // down an auth rabbit hole for a container that is simply broken. The
        // authoritative answer comes from `claude auth status --json` anyway;
        // this is only the fast path.
        let lower = stderr.to_ascii_lowercase();
        for marker in ["not logged in", "invalid api key", "please run /login"] {
            if lower.contains(marker) {
                return StartupFailure::NeedsAuth;
            }
        }

        StartupFailure::Misconfigured(match code {
            Some(c) if !stderr.trim().is_empty() => {
                format!("harness exited {c}: {}", stderr.trim())
            }
            Some(c) => format!("harness exited {c} with no output"),
            None => format!("harness terminated without an exit code: {}", stderr.trim()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn spec() -> SpawnSpec {
        SpawnSpec {
            node_id: uuid::Uuid::nil(),
            node_name: "worker".into(),
            model: None,
            prompt_file: PathBuf::from("/data/run/n/prompt.txt"),
            mcp_config: None,
            resume: None,
            config_dir: PathBuf::from("/data/creds/n"),
            cwd: PathBuf::from("/data"),
        }
    }

    fn argv_strings(s: &SpawnSpec) -> Vec<String> {
        ClaudeDriver
            .argv(s)
            .into_iter()
            .map(|o| o.to_string_lossy().into_owned())
            .collect()
    }

    /// Credentials reach the child from `auth::credential_env` and nowhere
    /// else. If a driver ever also set one of these, two variables would be
    /// live at once and which won would be the harness's business, not ours.
    #[test]
    fn the_driver_sets_no_credential_variables_of_its_own() {
        let spec = spec();
        let names: Vec<String> = ClaudeDriver
            .env(&spec)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        for var in [
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CODEX_API_KEY",
        ] {
            assert!(
                !names.iter().any(|k| k == var),
                "{var} must come from stored credentials, not the driver"
            );
        }
    }

    #[test]
    fn argv_matches_the_documented_invocation() {
        let a = argv_strings(&spec());
        assert_eq!(
            a,
            vec![
                "--print",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "bypassPermissions",
                "--append-system-prompt-file",
                "/data/run/n/prompt.txt",
            ]
        );
    }

    #[test]
    fn the_prompt_is_never_passed_inline() {
        // argv is world-readable across uids and the preamble carries injected
        // ctx, so the only acceptable form is a path.
        let a = argv_strings(&spec());
        assert!(a.contains(&"--append-system-prompt-file".to_string()));
        assert!(
            !a.contains(&"--append-system-prompt".to_string()),
            "the inline flag must never be used"
        );
    }

    #[test]
    fn optional_flags_appear_only_when_configured() {
        let mut s = spec();
        assert!(!argv_strings(&s).contains(&"--model".to_string()));
        assert!(!argv_strings(&s).contains(&"--resume".to_string()));
        assert!(!argv_strings(&s).contains(&"--mcp-config".to_string()));

        s.model = Some("opus".into());
        s.resume = Some("sess-1".into());
        s.mcp_config = Some(PathBuf::from("/data/run/n/mcp.json"));
        let a = argv_strings(&s);
        assert!(a.windows(2).any(|w| w == ["--model", "opus"]));
        assert!(a.windows(2).any(|w| w == ["--resume", "sess-1"]));
        assert!(a
            .windows(2)
            .any(|w| w == ["--mcp-config", "/data/run/n/mcp.json"]));
    }

    #[test]
    fn each_node_gets_its_own_config_dir_so_agents_can_be_different_accounts() {
        let env = ClaudeDriver.env(&spec());
        let get = |k: &str| {
            env.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(get("CLAUDE_CONFIG_DIR"), "/data/creds/n");
        assert_eq!(get("HOME"), "/data/creds/n");
        assert_eq!(get("IS_SANDBOX"), "1");
        // The engine secret must never reach a child.
        assert!(!env.iter().any(|(k, _)| k.contains("ENGINE_SECRET")));
    }

    #[test]
    fn a_turn_is_exactly_one_newline_terminated_json_line() {
        let line = ClaudeDriver.encode_turn("<AgentPrompt id=\"1\">\nhi\n</AgentPrompt>");
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
        let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(
            v["message"]["content"][0]["text"],
            "<AgentPrompt id=\"1\">\nhi\n</AgentPrompt>"
        );
    }

    #[test]
    fn init_result_and_assistant_are_recognised() {
        let init = ClaudeDriver
            .parse_line(r#"{"type":"system","subtype":"init","session_id":"s1","model":"opus"}"#);
        assert_eq!(
            init,
            HarnessEvent::Init {
                session_id: "s1".into()
            }
        );

        let asst = ClaudeDriver.parse_line(
            r#"{"type":"assistant","session_id":"s1","message":{"content":[{"type":"text","text":"hello"}]}}"#,
        );
        assert_eq!(
            asst,
            HarnessEvent::Text {
                session_id: Some("s1".into()),
                text: "hello".into()
            }
        );

        let res =
            ClaudeDriver.parse_line(r#"{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"s1"}"#);
        assert_eq!(
            res,
            HarnessEvent::Result {
                session_id: Some("s1".into()),
                is_error: false,
                text: Some("done".into())
            }
        );
    }

    /// QA's `<<FAKE:GARBAGE>>` and `<<FAKE:NOISE>>` cases: a parser that
    /// pattern-matched exhaustively on event type would fall over in production.
    #[test]
    fn unknown_events_and_non_json_lines_are_never_fatal() {
        for line in [
            "not json at all",
            "",
            "   ",
            "{",
            r#"{"type":"rate_limit_event","limit":100}"#,
            r#"{"type":"system","subtype":"thinking_tokens","n":5}"#,
            r#"{"no_type_field":true}"#,
            r#"[1,2,3]"#,
        ] {
            assert!(
                matches!(ClaudeDriver.parse_line(line), HarnessEvent::Unknown { .. }),
                "{line:?} should parse as Unknown, not panic or error"
            );
        }
    }

    #[test]
    fn an_init_without_a_session_id_is_not_treated_as_an_init() {
        // F008 keys turn-completion on session_id, so an init we cannot bind to
        // a session is useless and must not set one.
        let e = ClaudeDriver.parse_line(r#"{"type":"system","subtype":"init","model":"opus"}"#);
        assert!(matches!(e, HarnessEvent::Unknown { .. }));
    }

    #[test]
    fn a_root_refusal_is_misconfiguration_not_needs_auth() {
        // Both exit 1; only stderr distinguishes them. Getting this wrong makes
        // every container report needs_auth forever.
        let root = ClaudeDriver.classify_startup_failure(
            Some(1),
            "--dangerously-skip-permissions cannot be used with root/sudo privileges for security reasons",
        );
        assert!(matches!(root, StartupFailure::Misconfigured(_)));

        // A recognised auth message, and only that, means needs_auth.
        assert_eq!(
            ClaudeDriver.classify_startup_failure(Some(1), "Not logged in · Please run /login"),
            StartupFailure::NeedsAuth
        );
    }
}

#[cfg(test)]
mod startup_failure_tests {
    use super::*;

    /// The trap that would otherwise report every misconfigured container as
    /// needing auth forever: running as root and being logged out BOTH exit 1,
    /// and the root refusal writes NOTHING to stdout.
    #[test]
    fn the_root_refusal_is_misconfiguration_not_needs_auth() {
        let f = ClaudeDriver.classify_startup_failure(
            Some(1),
            "--dangerously-skip-permissions cannot be used with root/sudo privileges for security reasons",
        );
        match f {
            StartupFailure::Misconfigured(m) => {
                assert!(m.contains("non-root") || m.contains("IS_SANDBOX"));
            }
            other => panic!("root refusal must not be {other:?}"),
        }
    }

    #[test]
    fn genuine_auth_failures_are_recognised() {
        for stderr in [
            "Not logged in · Please run /login",
            "Invalid API key · Please run /login",
            "NOT LOGGED IN",
        ] {
            assert_eq!(
                ClaudeDriver.classify_startup_failure(Some(1), stderr),
                StartupFailure::NeedsAuth,
                "{stderr:?} should be needs_auth"
            );
        }
    }

    /// An unrecognised failure must NOT be guessed as needs_auth: that would
    /// send the operator down an auth rabbit hole for a broken container.
    #[test]
    fn an_unrecognised_failure_defaults_to_misconfigured() {
        for (code, stderr) in [
            (Some(127), "claude: command not found"),
            (Some(2), "some new error we have never seen"),
            (None, "killed"),
            (Some(1), ""),
        ] {
            assert!(
                matches!(
                    ClaudeDriver.classify_startup_failure(code, stderr),
                    StartupFailure::Misconfigured(_)
                ),
                "{stderr:?} must not be guessed as needs_auth"
            );
        }
    }
}
