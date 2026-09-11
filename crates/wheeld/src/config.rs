//! Settings for the single-process daemon: two flags, and defaults for everything else.

use crate::tokens::TokenCommand;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;

pub const USAGE: &str = "\
wheeld — Wheel in one process: API, sandbox host, and per-project engines.

USAGE:
    wheeld [--data-dir <path>] [--bind <addr>]
    wheeld token create [--name <label>] [--email <account>] [--data-dir <path>]
    wheeld token list [--data-dir <path>]
    wheeld token revoke <id> [--data-dir <path>]

OPTIONS:
    --data-dir <path>   Where boards, secrets and project data live.
                        Default: $WHEEL_DATA_DIR, else ~/.wheel
    --bind <addr>       Address to serve on. Default: $BIND_ADDR, else 127.0.0.1:8080.
                        Any other address is reachable from other machines, and says so.
    -h, --help          Print this message
    -V, --version       Print the version

The first start writes an operator token to <data-dir>/operator-token. Send it as
`x-auth-token: <token>` or `Authorization: Bearer <token>`. `wheeld token` makes more,
lists them and revokes them, straight from the data directory.

ENVIRONMENT:
    WHEEL_ALLOWED_HOSTS   More host names a request may be addressed to, besides localhost
                          and IP addresses. Refusing the rest keeps DNS-rebinding pages out.
    WHEEL_SIGNUP          closed (the default) or open. Closed, the owner adds
                          accounts with POST /v1/auth/users and the operator token.
    CORS_ALLOWED_ORIGINS  Browser origins allowed to call the API directly. Default: none.
";

/// Loopback: only this machine can reach it until the operator says otherwise.
pub const DEFAULT_BIND: &str = "127.0.0.1:8080";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub data_dir: PathBuf,
    pub bind: String,
}

/// What `main` should do, decided from the arguments before anything is started.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Run(Settings),
    Token {
        data_dir: PathBuf,
        command: TokenCommand,
    },
    PrintUsage,
    PrintVersion,
}

impl Settings {
    /// Parse arguments (excluding argv[0]).
    ///
    /// Environment variables supply the defaults, so `wheeld` with no flags works and a
    /// containerised deployment can configure it the way it configures everything else.
    pub fn parse<I, S>(args: I) -> Result<Action>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let args: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
        if args.first().map(String::as_str) == Some("token") {
            return parse_token(&args[1..]);
        }

        let mut data_dir: Option<PathBuf> = None;
        let mut bind: Option<String> = None;
        let mut it = args.into_iter();

        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-h" | "--help" => return Ok(Action::PrintUsage),
                "-V" | "--version" => return Ok(Action::PrintVersion),
                "--data-dir" => {
                    let v = it.next().context("--data-dir needs a path")?;
                    data_dir = Some(PathBuf::from(v));
                }
                "--bind" => {
                    bind = Some(it.next().context("--bind needs an address")?);
                }
                other => {
                    if let Some(v) = other.strip_prefix("--data-dir=") {
                        data_dir = Some(PathBuf::from(v));
                    } else if let Some(v) = other.strip_prefix("--bind=") {
                        bind = Some(v.to_string());
                    } else {
                        bail!("unknown argument {other:?}\n\n{USAGE}");
                    }
                }
            }
        }

        let data_dir = resolve_data_dir(data_dir)?;
        let bind = bind
            .or_else(|| std::env::var("BIND_ADDR").ok())
            .unwrap_or_else(|| DEFAULT_BIND.to_string());

        Ok(Action::Run(Settings { data_dir, bind }))
    }
}

/// `wheeld token create|list|revoke`, with the same `--data-dir` rules as the daemon.
fn parse_token(args: &[String]) -> Result<Action> {
    let mut it = args.iter();
    let sub = it.next().map(String::as_str);
    let (mut data_dir, mut name, mut email) = (None, None, None);
    let mut positional: Vec<&str> = Vec::new();

    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = |what: &str| -> Result<String> {
            match &inline {
                Some(v) => Ok(v.clone()),
                None => it
                    .next()
                    .cloned()
                    .with_context(|| format!("{flag} needs {what}")),
            }
        };
        match flag {
            "-h" | "--help" => return Ok(Action::PrintUsage),
            "--data-dir" => data_dir = Some(PathBuf::from(value("a path")?)),
            "--name" => name = Some(value("a label")?),
            "--email" => email = Some(value("an account's email")?),
            other if other.starts_with('-') => bail!("unknown argument {other:?}\n\n{USAGE}"),
            _ => positional.push(arg),
        }
    }

    if sub != Some("create") && (name.is_some() || email.is_some()) {
        bail!("--name and --email are options of `wheeld token create`");
    }
    let command = match (sub, positional.as_slice()) {
        (Some("create"), []) => TokenCommand::Create {
            name: name.unwrap_or_else(|| "cli".to_string()),
            email,
        },
        (Some("list"), []) => TokenCommand::List,
        (Some("revoke"), [id]) => TokenCommand::Revoke { id: id.to_string() },
        (Some("revoke"), _) => bail!("`wheeld token revoke` takes exactly one token id"),
        _ => bail!("expected `wheeld token create|list|revoke`\n\n{USAGE}"),
    };
    Ok(Action::Token {
        data_dir: resolve_data_dir(data_dir)?,
        command,
    })
}

fn resolve_data_dir(flag: Option<PathBuf>) -> Result<PathBuf> {
    flag.or_else(|| std::env::var("WHEEL_DATA_DIR").ok().map(PathBuf::from))
        .map(Ok)
        .unwrap_or_else(default_data_dir)
}

/// `~/.wheel`, or an error that says what to pass instead.
///
/// Falling back to the working directory would scatter a `.wheel` beside whatever the user happened
/// to be in, and each one would look like a different install with different projects.
fn default_data_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .context("no HOME to put the data directory in — pass --data-dir")?;
    Ok(PathBuf::from(home).join(".wheel"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Settings {
        match Settings::parse(args).unwrap() {
            Action::Run(s) => s,
            other => panic!("expected a run, got {other:?}"),
        }
    }

    /// Every case that reads or writes the environment, in one test.
    ///
    /// Environment variables are process-global and Rust runs tests in parallel threads, so these
    /// as separate `#[test]`s race: one clears `WHEEL_DATA_DIR` while another sets it, and which
    /// one wins depends on timing. It failed that way once here before being noticed, which is the
    /// worst kind of test — it passes on rerun and teaches you to ignore it. Sequencing the cases
    /// inside a single test makes the interference impossible rather than unlikely. Same reasoning,
    /// and same shape, as wheel-api's config_interlock.
    #[test]
    fn configuration_comes_from_flags_then_environment_then_defaults() {
        fn clear() {
            std::env::remove_var("WHEEL_DATA_DIR");
            std::env::remove_var("BIND_ADDR");
        }

        // Nothing set: the defaults are a working configuration on their own, which is the whole
        // promise of "zero flags" — and the default bind is loopback, so zero flags also means
        // nothing but this machine can reach it (docs/proposals/headless-first.md).
        clear();
        std::env::set_var("HOME", "/home/someone");
        let s = run(&[]);
        assert_eq!(s.data_dir, PathBuf::from("/home/someone/.wheel"));
        assert_eq!(s.bind, "127.0.0.1:8080");

        // The environment supplies defaults when no flags are given.
        std::env::set_var("WHEEL_DATA_DIR", "/from/env");
        std::env::set_var("BIND_ADDR", "0.0.0.0:9999");
        let s = run(&[]);
        assert_eq!(s.data_dir, PathBuf::from("/from/env"));
        assert_eq!(s.bind, "0.0.0.0:9999");

        // ...and a flag overrides it.
        let s = run(&["--data-dir", "/from/flag", "--bind", "127.0.0.1:1234"]);
        assert_eq!(s.data_dir, PathBuf::from("/from/flag"));
        assert_eq!(s.bind, "127.0.0.1:1234");

        // The --flag=value form is equivalent.
        clear();
        let s = run(&["--data-dir=/x", "--bind=[::1]:80"]);
        assert_eq!(s.data_dir, PathBuf::from("/x"));
        assert_eq!(s.bind, "[::1]:80");

        // The token subcommands find the data directory by the same rules.
        std::env::set_var("WHEEL_DATA_DIR", "/from/env");
        assert_eq!(
            Settings::parse(["token", "list"]).unwrap(),
            Action::Token {
                data_dir: PathBuf::from("/from/env"),
                command: TokenCommand::List
            }
        );

        // No HOME and no flag: there is nothing to derive a data directory from, so the error has
        // to name the flag that fixes it rather than invent a location.
        clear();
        std::env::remove_var("HOME");
        let e = Settings::parse::<[&str; 0], &str>([]).unwrap_err();
        assert!(format!("{e:#}").contains("--data-dir"), "{e:#}");

        clear();
        std::env::set_var("HOME", "/home/someone");
    }

    #[test]
    fn help_and_version_are_actions_not_side_effects() {
        assert_eq!(Settings::parse(["--help"]).unwrap(), Action::PrintUsage);
        assert_eq!(Settings::parse(["-h"]).unwrap(), Action::PrintUsage);
        assert_eq!(Settings::parse(["-V"]).unwrap(), Action::PrintVersion);
        assert_eq!(
            Settings::parse(["token", "create", "--help"]).unwrap(),
            Action::PrintUsage
        );
    }

    /// A mistyped flag must not be ignored: silently running with a default the user did not ask
    /// for is how data ends up in the wrong directory.
    #[test]
    fn an_unknown_flag_is_refused_and_the_message_says_how_to_use_it() {
        let e = Settings::parse(["--datadir", "/x"]).unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("--datadir"), "{msg}");
        assert!(msg.contains("USAGE"), "{msg}");
    }

    #[test]
    fn a_flag_without_its_value_is_an_error() {
        assert!(Settings::parse(["--data-dir"]).is_err());
        assert!(Settings::parse(["--bind"]).is_err());
        assert!(Settings::parse(["token", "create", "--name"]).is_err());
    }

    fn token(args: &[&str]) -> TokenCommand {
        let mut all = vec!["token"];
        all.extend_from_slice(args);
        all.extend_from_slice(&["--data-dir", "/d"]);
        match Settings::parse(all).unwrap() {
            Action::Token { data_dir, command } => {
                assert_eq!(data_dir, PathBuf::from("/d"));
                command
            }
            other => panic!("expected a token command, got {other:?}"),
        }
    }

    #[test]
    fn token_subcommands_parse() {
        assert_eq!(
            token(&["create"]),
            TokenCommand::Create {
                name: "cli".into(),
                email: None
            }
        );
        assert_eq!(
            token(&["create", "--name", "ci", "--email=me@example.com"]),
            TokenCommand::Create {
                name: "ci".into(),
                email: Some("me@example.com".into())
            }
        );
        assert_eq!(token(&["list"]), TokenCommand::List);
        assert_eq!(
            token(&["revoke", "0b0e"]),
            TokenCommand::Revoke { id: "0b0e".into() }
        );
    }

    #[test]
    fn a_malformed_token_command_is_refused_with_a_reason() {
        for (args, says) in [
            (vec!["token"], "create|list|revoke"),
            (vec!["token", "rotate"], "create|list|revoke"),
            (vec!["token", "revoke"], "exactly one"),
            (vec!["token", "revoke", "a", "b"], "exactly one"),
            (vec!["token", "list", "extra"], "create|list|revoke"),
            (vec!["token", "list", "--name", "x"], "wheeld token create"),
            (vec!["token", "create", "--bogus"], "--bogus"),
        ] {
            let e = Settings::parse(args.clone()).unwrap_err();
            assert!(format!("{e:#}").contains(says), "{args:?}: {e:#}");
        }
    }
}

#[cfg(test)]
mod ready_line_tests {
    use super::super::displayable;

    /// The first line a new user reads. `0.0.0.0:8080` is not something a browser can open.
    #[test]
    fn the_ready_line_gives_an_address_a_browser_can_open() {
        assert_eq!(displayable("0.0.0.0:8080"), "localhost:8080");
        assert_eq!(displayable("[::]:8080"), "localhost:8080");
        assert_eq!(displayable("127.0.0.1:8099"), "127.0.0.1:8099");
    }
}
